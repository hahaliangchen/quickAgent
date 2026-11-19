//! Built-in zero-I/O paging methods for Lazy Resource Handle objects
//! (`io.read` / `io.read_lines` / `io.read_chunk` / `process.run` outputs):
//! - `.slice(startLine, endLine?)` or multi-range `.slice([[s1, e1], [s2, e2]])`
//! - `.grep(pattern, contextLines?)` (case-insensitive string or `/regex/`)
//! - `.outline()` (structural symbol/heading outline with 1-based line numbers)
//! - `.head(n?)` / `.tail(n?)`
//!
//! Keeps `Object.prototype` untouched: returns `Ok(None)` for ordinary JS objects
//! so standard ECMAScript `TypeError` semantics stay 100% intact.

use super::heap::{Heap, HeapId, HeapNode, VmValue};
use super::js_value::{integer_or_infinity, to_number, to_string};
use anyhow::{Result, bail, ensure};

const TEXT_RESULT_LIMIT: usize = 4 * 1024 * 1024;
const MAX_GREP_LINES: usize = 60;

fn arg(args: &[VmValue], index: usize) -> VmValue {
    args.get(index).cloned().unwrap_or(VmValue::Undefined)
}

fn array_items(heap: &Heap, id: HeapId) -> Result<Vec<VmValue>> {
    match heap.get(id)? {
        HeapNode::Array(items) => Ok(items.clone()),
        _ => bail!("TypeError: receiver is not an array"),
    }
}

fn regex_parts(heap: &Heap, value: &VmValue) -> Option<(String, String)> {
    match value {
        VmValue::Reference(id) => heap.regex_parts(*id),
        _ => None,
    }
}

fn extract_handle_text(heap: &Heap, id: HeapId) -> Result<Option<String>> {
    let HeapNode::Object(props) = heap.get(id)? else {
        return Ok(None);
    };
    let prop_str = |key: &str| -> Option<String> {
        props.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
            VmValue::String(s) => Some(s.clone()),
            _ => None,
        })
    };
    let has_key = |key: &str| props.iter().any(|(k, _)| k == key);
    if let Some(content) = prop_str("content")
        && (has_key("path") || has_key("__ref") || has_key("complete"))
    {
        return Ok(Some(content));
    }
    if has_key("exit_code") && (has_key("stdout") || has_key("stderr")) {
        let stdout = prop_str("stdout").unwrap_or_default();
        let stderr = prop_str("stderr").unwrap_or_default();
        let combined = if stderr.is_empty() {
            stdout
        } else if stdout.is_empty() {
            stderr
        } else {
            format!("{stdout}\n--- stderr ---\n{stderr}")
        };
        return Ok(Some(combined));
    }
    Ok(None)
}

pub(super) fn try_method(
    heap: &mut Heap,
    id: HeapId,
    name: &str,
    args: &[VmValue],
) -> Result<Option<VmValue>> {
    if !matches!(name, "slice" | "grep" | "outline" | "head" | "tail") {
        return Ok(None);
    }
    let Some(text) = extract_handle_text(heap, id)? else {
        return Ok(None);
    };

    let lines: Vec<&str> = text.lines().collect();
    let out = match name {
        "slice" => eval_slice(heap, &lines, args)?,
        "head" => {
            let n = match arg(args, 0) {
                VmValue::Undefined => 30usize,
                other => (integer_or_infinity(to_number(heap, &other)?) as usize).clamp(1, 200),
            };
            format_range(&lines, 1, n)
        }
        "tail" => {
            let n = match arg(args, 0) {
                VmValue::Undefined => 30usize,
                other => (integer_or_infinity(to_number(heap, &other)?) as usize).clamp(1, 200),
            };
            let start = lines.len().saturating_sub(n).saturating_add(1).max(1);
            format_range(&lines, start, lines.len())
        }
        "grep" => eval_grep(heap, &lines, args)?,
        "outline" => eval_outline(&lines),
        _ => return Ok(None),
    };

    ensure!(
        out.len() <= TEXT_RESULT_LIMIT,
        "RangeError: string result exceeds 4 MiB"
    );
    Ok(Some(VmValue::String(out)))
}

fn format_range(lines: &[&str], start_1: usize, end_1: usize) -> String {
    let total = lines.len();
    if total == 0 {
        return String::new();
    }
    let s = start_1.max(1).min(total);
    let e = end_1.max(s).min(total);
    (s..=e)
        .map(|n| format!("{n}: {}", lines[n - 1]))
        .collect::<Vec<_>>()
        .join("\n")
}

fn eval_slice(heap: &Heap, lines: &[&str], args: &[VmValue]) -> Result<String> {
    let total = lines.len();
    let first = arg(args, 0);
    if let VmValue::Reference(arr_id) = first
        && matches!(heap.get(arr_id)?, HeapNode::Array(_))
    {
        let items = array_items(heap, arr_id)?;
        let mut ranges = Vec::new();
        for item in items {
            let VmValue::Reference(pair_id) = item else {
                bail!("TypeError: resource.slice(ranges) expects an array of [startLine, endLine] pairs");
            };
            let pair = array_items(heap, pair_id)?;
            ensure!(
                !pair.is_empty(),
                "TypeError: resource.slice range pair must have at least [startLine]"
            );
            let s = integer_or_infinity(to_number(heap, &pair[0])?).max(1.0) as usize;
            let e = if pair.len() > 1 {
                integer_or_infinity(to_number(heap, &pair[1])?).max(s as f64) as usize
            } else {
                s.saturating_add(59)
            };
            ranges.push((s, e));
        }
        let mut chunks = Vec::new();
        let mut prev_end: Option<usize> = None;
        for (s, e) in ranges {
            if total == 0 || s > total {
                continue;
            }
            let cs = s.max(1);
            let ce = e.min(total);
            if let Some(pe) = prev_end
                && cs > pe + 1
            {
                chunks.push(format!("// ... (skipped {} lines) ...", cs - pe - 1));
            }
            chunks.push(format_range(lines, cs, ce));
            prev_end = Some(ce);
        }
        return Ok(chunks.join("\n"));
    }

    let start = match first {
        VmValue::Undefined => 1usize,
        other => integer_or_infinity(to_number(heap, &other)?).max(1.0) as usize,
    };
    let end = match arg(args, 1) {
        VmValue::Undefined => start.saturating_add(59),
        other => integer_or_infinity(to_number(heap, &other)?).max(start as f64) as usize,
    };
    if total == 0 || start > total {
        Ok(String::new())
    } else {
        Ok(format_range(lines, start, end))
    }
}

fn eval_grep(heap: &Heap, lines: &[&str], args: &[VmValue]) -> Result<String> {
    let total = lines.len();
    let pat = arg(args, 0);
    ensure!(
        !matches!(pat, VmValue::Undefined | VmValue::Null),
        "TypeError: resource.grep(pattern, contextLines?) requires a pattern"
    );
    let ctx = match arg(args, 1) {
        VmValue::Undefined => 1usize,
        other => (integer_or_infinity(to_number(heap, &other)?) as usize).min(10),
    };
    let matched: Vec<usize> = if let Some((pattern, flags)) = regex_parts(heap, &pat) {
        let re = super::values::compile_regex(&pattern, &flags)?;
        lines
            .iter()
            .enumerate()
            .filter(|(_, line)| re.is_match(line).unwrap_or(false))
            .map(|(i, _)| i)
            .collect()
    } else {
        let needle = to_string(heap, &pat)?.to_lowercase();
        if needle.is_empty() {
            Vec::new()
        } else {
            lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line.to_lowercase().contains(&needle))
                .map(|(i, _)| i)
                .collect()
        }
    };
    if matched.is_empty() {
        return Ok("(no matches)".to_owned());
    }
    let mut windows: Vec<(usize, usize)> = Vec::new();
    for idx in &matched {
        let s = idx.saturating_sub(ctx);
        let e = (idx + ctx).min(total.saturating_sub(1));
        if let Some(last) = windows.last_mut()
            && s <= last.1 + 1
        {
            last.1 = last.1.max(e);
        } else {
            windows.push((s, e));
        }
    }
    let mut out_lines = Vec::new();
    let mut truncated = 0usize;
    for (w_idx, (s, e)) in windows.iter().enumerate() {
        if w_idx > 0 && out_lines.len() < MAX_GREP_LINES {
            out_lines.push("--".to_owned());
        }
        for i in *s..=*e {
            if out_lines.len() < MAX_GREP_LINES {
                out_lines.push(format!("{}: {}", i + 1, lines[i]));
            } else {
                truncated += 1;
            }
        }
    }
    if truncated > 0 {
        out_lines.push(format!(
            "// ... (+{truncated} more lines across {} total matches)",
            matched.len()
        ));
    }
    Ok(out_lines.join("\n"))
}

fn eval_outline(lines: &[&str]) -> String {
    let mut entries = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let is_symbol = trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub async fn ")
            || trimmed.starts_with("pub(crate) fn ")
            || trimmed.starts_with("pub(super) fn ")
            || trimmed.starts_with("async fn ")
            || trimmed.starts_with("fn ")
            || trimmed.starts_with("pub struct ")
            || trimmed.starts_with("struct ")
            || trimmed.starts_with("pub enum ")
            || trimmed.starts_with("enum ")
            || trimmed.starts_with("pub trait ")
            || trimmed.starts_with("trait ")
            || trimmed.starts_with("impl ")
            || trimmed.starts_with("impl<")
            || trimmed.starts_with("pub mod ")
            || trimmed.starts_with("mod ")
            || trimmed.starts_with("export ")
            || trimmed.starts_with("class ")
            || trimmed.starts_with("interface ")
            || trimmed.starts_with("type ")
            || trimmed.starts_with("function ")
            || trimmed.starts_with("async function ")
            || trimmed.starts_with("def ")
            || trimmed.starts_with("func ")
            || trimmed.starts_with("# ")
            || trimmed.starts_with("## ")
            || trimmed.starts_with("### ");
        if is_symbol {
            let preview: String = trimmed.chars().take(120).collect();
            entries.push(format!("{}: {}", idx + 1, preview));
            if entries.len() >= 80 {
                entries.push("// ... (outline capped at 80 entries)".to_owned());
                break;
            }
        }
    }
    if entries.is_empty() {
        format!(
            "(no structural outline detected in {} lines; use .head(), .slice(start, end), or .grep(pattern))",
            lines.len()
        )
    } else {
        entries.join("\n")
    }
}
