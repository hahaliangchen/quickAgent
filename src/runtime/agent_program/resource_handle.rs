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
    if let Some(res) = try_blueprint_method(heap, id, name, args)? {
        return Ok(Some(res));
    }
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
        .map(|n| format!("{n}|{}", lines[n - 1]))
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
                out_lines.push(format!("{}|{}", i + 1, lines[i]));
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
            entries.push(format!("{}|{}", idx + 1, preview));
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

// ============================================================================
// Live Blueprint Class & Method-Chain Handle (`const bp = blueprint.open({})`)
// ============================================================================

fn push_unique_str(list: &mut Vec<serde_json::Value>, val: &str) {
    let trimmed = val.trim();
    if trimmed.is_empty() {
        return;
    }
    if !list.iter().any(|v| v.as_str() == Some(trimmed)) {
        list.push(serde_json::Value::String(trimmed.to_owned()));
    }
}

fn try_blueprint_method(
    heap: &mut Heap,
    id: HeapId,
    name: &str,
    args: &[VmValue],
) -> Result<Option<VmValue>> {
    let (is_graph, is_node) = match heap.get(id)? {
        HeapNode::Object(props) => (
            props
                .iter()
                .any(|(k, v)| k == "__blueprint_graph" && *v == VmValue::Boolean(true)),
            props
                .iter()
                .any(|(k, v)| k == "__blueprint_node" && *v == VmValue::Boolean(true)),
        ),
        _ => return Ok(None),
    };

    if is_graph {
        return eval_blueprint_graph_method(heap, id, name, args);
    }
    if is_node {
        return eval_blueprint_node_method(heap, id, name, args);
    }
    Ok(None)
}

fn eval_blueprint_graph_method(
    heap: &mut Heap,
    id: HeapId,
    name: &str,
    args: &[VmValue],
) -> Result<Option<VmValue>> {
    let root = heap.to_json(&VmValue::Reference(id))?;
    let class_map_id = match heap.get(id)? {
        HeapNode::Object(props) => props.iter().find_map(|(k, v)| match (k.as_str(), v) {
            ("class_map", VmValue::Reference(cid)) => Some(*cid),
            _ => None,
        }),
        _ => None,
    };
    match name {
        "chains" => Ok(Some(heap.from_json(&root["chain_list"])?)),
        "files" => Ok(Some(heap.from_json(&root["file_list"])?)),
        "classes" => {
            let mut items = Vec::new();
            if let Some(map) = root["class_map"].as_object() {
                for (cls_name, cls) in map {
                    let methods: Vec<String> = cls["method_map"]
                        .as_object()
                        .map(|m| m.keys().cloned().collect())
                        .unwrap_or_default();
                    let props: Vec<String> = cls["properties"]
                        .as_object()
                        .map(|p| p.keys().cloned().collect())
                        .unwrap_or_default();
                    items.push(serde_json::json!({
                        "class_name": cls_name,
                        "id": cls["id"],
                        "name": cls["name"],
                        "kind": cls["kind"],
                        "summary": cls["description"],
                        "properties": props,
                        "methods": methods,
                    }));
                }
            }
            Ok(Some(heap.from_json(&serde_json::Value::Array(items))?))
        }
        "class" => {
            let query = to_string(heap, &arg(args, 0))?;
            let q = query.trim();
            if q.is_empty() {
                return Ok(Some(VmValue::Undefined));
            }
            let Some(map) = root["class_map"].as_object() else {
                return Ok(Some(VmValue::Undefined));
            };
            let found = map.get(q).or_else(|| {
                map.values().find(|cls| {
                    cls["id"].as_str() == Some(q)
                        || cls["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(q))
                        || cls["class_name"]
                            .as_str()
                            .is_some_and(|n| n.eq_ignore_ascii_case(q))
                })
            });
            let Some(mut node) = found.cloned() else {
                return Ok(Some(VmValue::Undefined));
            };
            if let Some(cid) = class_map_id
                && let Some(obj) = node.as_object_mut()
            {
                obj.insert("__bp_classes_id".into(), serde_json::json!(cid.raw()));
            }
            Ok(Some(heap.from_json(&node)?))
        }
        "find" => {
            let raw_query = to_string(heap, &arg(args, 0))?;
            let q = raw_query.trim().to_ascii_lowercase();
            if q.is_empty() {
                return Ok(Some(heap.from_json(&serde_json::json!({
                    "query": "",
                    "classes": [],
                    "chains": [],
                }))?));
            }
            let mut matched_classes = Vec::new();
            if let Some(map) = root["class_map"].as_object() {
                for cls in map.values() {
                    let hay = serde_json::to_string(cls).unwrap_or_default().to_ascii_lowercase();
                    if hay.contains(&q) {
                        let mut node = cls.clone();
                        if let Some(cid) = class_map_id
                            && let Some(obj) = node.as_object_mut()
                        {
                            obj.insert("__bp_classes_id".into(), serde_json::json!(cid.raw()));
                        }
                        matched_classes.push(node);
                    }
                }
            }
            let mut matched_chains = Vec::new();
            if let Some(chains) = root["chain_list"].as_array() {
                for ch in chains {
                    let hay = serde_json::to_string(ch).unwrap_or_default().to_ascii_lowercase();
                    if hay.contains(&q) {
                        matched_chains.push(ch.clone());
                    }
                }
            }
            Ok(Some(heap.from_json(&serde_json::json!({
                "query": raw_query.trim(),
                "classes": matched_classes,
                "chains": matched_chains,
            }))?))
        }
        _ => Ok(None),
    }
}

fn eval_blueprint_node_method(
    heap: &mut Heap,
    id: HeapId,
    name: &str,
    args: &[VmValue],
) -> Result<Option<VmValue>> {
    let node = heap.to_json(&VmValue::Reference(id))?;
    match name {
        "props" => return Ok(Some(heap.from_json(&node["properties"])?)),
        "anchors" => return Ok(Some(heap.from_json(&node["chain_anchors"])?)),
        "invariants" => return Ok(Some(heap.from_json(&node["chain_invariants"])?)),
        "trace" => return Ok(Some(heap.from_json(&node["chain_trace"])?)),
        "branches" => return Ok(Some(heap.from_json(&node["chain_branches"])?)),
        "methods" => {
            let mut list = Vec::new();
            if let Some(map) = node["method_map"].as_object() {
                for (js_name, m) in map {
                    let targets: Vec<String> = m["next"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|e| {
                            Some(format!(
                                "{}.{}",
                                e["target_class"].as_str()?,
                                e["target_method"].as_str()?
                            ))
                        })
                        .collect();
                    list.push(serde_json::json!({
                        "name": js_name,
                        "id": m["id"],
                        "label": m["name"],
                        "reads": m["reads"],
                        "summary": m["summary"],
                        "next": targets,
                    }));
                }
            }
            return Ok(Some(heap.from_json(&serde_json::Value::Array(list))?));
        }
        "summary" => {
            let out = serde_json::json!({
                "class": node["class_name"],
                "id": node["id"],
                "kind": node["kind"],
                "description": node["description"],
                "properties": node["properties"],
                "trace": node["chain_trace"],
                "anchors": node["chain_anchors"],
                "invariants": node["chain_invariants"],
                "branches": node["chain_branches"],
            });
            return Ok(Some(heap.from_json(&out)?));
        }
        _ => {}
    }

    // Dynamic class method call: match by `js_name`, `id`, or display `name`.
    let Some(method_map) = node["method_map"].as_object() else {
        return Ok(None);
    };
    let method = method_map.get(name).or_else(|| {
        method_map.values().find(|m| {
            m["id"].as_str() == Some(name) || m["name"].as_str() == Some(name)
        })
    });
    let Some(method) = method else {
        return Ok(None);
    };

    let cur_cls = node["class_name"]
        .as_str()
        .or_else(|| node["name"].as_str())
        .unwrap_or("Class")
        .to_owned();
    let m_js = method["js_name"].as_str().unwrap_or(name).to_owned();

    let mut trace = node["chain_trace"].as_array().cloned().unwrap_or_default();
    let mut anchors = node["chain_anchors"].as_array().cloned().unwrap_or_default();
    let mut invariants = node["chain_invariants"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    for f in node["anchor_files"].as_array().into_iter().flatten() {
        if let Some(s) = f.as_str() {
            push_unique_str(&mut anchors, s);
        }
    }
    for r in node["rules"].as_array().into_iter().flatten() {
        if let Some(s) = r.as_str() {
            push_unique_str(&mut invariants, s);
        }
    }
    // If method declares specific `reads`, include matching file/symbol/rule properties.
    if let Some(reads) = method["reads"].as_array()
        && let Some(prop_list) = node["property_list"].as_array()
    {
        for read_key in reads.iter().filter_map(serde_json::Value::as_str) {
            for prop in prop_list {
                let matches_prop = prop["id"].as_str() == Some(read_key)
                    || prop["name"].as_str() == Some(read_key);
                if !matches_prop {
                    continue;
                }
                let val = prop["value"].as_str().unwrap_or_default();
                match prop["kind"].as_str() {
                    Some("file") => push_unique_str(&mut anchors, val),
                    Some("symbol") => {
                        if let Some((f, _)) = val.split_once('#') {
                            push_unique_str(&mut anchors, f);
                        }
                    }
                    Some("rule") => push_unique_str(&mut invariants, val),
                    _ => {}
                }
            }
        }
    }

    // Optional branch selector argument: string or `{ target: "..." }`.
    let selector = match arg(args, 0) {
        VmValue::Undefined | VmValue::Null => None,
        VmValue::Reference(arg_id) => {
            let arg_json = heap.to_json(&VmValue::Reference(arg_id)).ok();
            arg_json
                .as_ref()
                .and_then(|v| v["target"].as_str().or_else(|| v.as_str()))
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        }
        other => to_string(heap, &other)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty()),
    };

    let next_edges = method["next"].as_array().cloned().unwrap_or_default();
    let chosen_edge = if let Some(sel) = selector.as_deref() {
        next_edges.iter().find(|e| {
            e["target_class"].as_str().is_some_and(|s| s.eq_ignore_ascii_case(sel))
                || e["target_class_id"].as_str() == Some(sel)
                || e["target_method"].as_str().is_some_and(|s| s.eq_ignore_ascii_case(sel))
                || e["target_method_id"].as_str() == Some(sel)
                || e["label"].as_str().is_some_and(|s| s.contains(sel))
        })
    } else {
        next_edges.first()
    };

    let branches_summary: Vec<serde_json::Value> = next_edges
        .iter()
        .map(|e| {
            serde_json::json!({
                "target_class": e["target_class"],
                "target_method": e["target_method"],
                "relation": e["relation"],
                "label": e["label"],
                "invariant": e["invariant"],
            })
        })
        .collect();

    let classes_raw = node["__bp_classes_id"].as_f64().map(|n| n as u64);
    if let Some(edge) = chosen_edge
        && let Some(classes_raw) = classes_raw
        && let Some(target_cls_name) = edge["target_class"].as_str()
    {
        let target_m = edge["target_method"].as_str().unwrap_or("call");
        let label = edge["label"].as_str().unwrap_or_default().trim();
        let step_text = if label.is_empty() {
            format!("{cur_cls}.{m_js} -> {target_cls_name}.{target_m}")
        } else {
            format!("{cur_cls}.{m_js} -[{label}]-> {target_cls_name}.{target_m}")
        };
        trace.push(serde_json::Value::String(step_text));
        if let Some(inv) = edge["invariant"].as_str() {
            push_unique_str(&mut invariants, inv);
        }

        let classes_json = heap.to_json(&VmValue::Reference(HeapId::from_raw(classes_raw)))?;
        if let Some(mut next_node) = classes_json.get(target_cls_name).cloned() {
            for f in next_node["anchor_files"].as_array().into_iter().flatten() {
                if let Some(s) = f.as_str() {
                    push_unique_str(&mut anchors, s);
                }
            }
            for r in next_node["rules"].as_array().into_iter().flatten() {
                if let Some(s) = r.as_str() {
                    push_unique_str(&mut invariants, s);
                }
            }
            if let Some(obj) = next_node.as_object_mut() {
                obj.insert("__bp_classes_id".into(), serde_json::json!(classes_raw));
                obj.insert("chain_trace".into(), serde_json::Value::Array(trace));
                obj.insert("chain_anchors".into(), serde_json::Value::Array(anchors));
                obj.insert(
                    "chain_invariants".into(),
                    serde_json::Value::Array(invariants),
                );
                obj.insert(
                    "chain_branches".into(),
                    serde_json::Value::Array(branches_summary),
                );
                obj.insert("current_method".into(), serde_json::json!(target_m));
            }
            return Ok(Some(heap.from_json(&next_node)?));
        }
    }

    // Leaf method (no outgoing edges): record step on current class and return updated node.
    trace.push(serde_json::Value::String(format!("{cur_cls}.{m_js}")));
    let mut leaf = node;
    if let Some(obj) = leaf.as_object_mut() {
        obj.insert("chain_trace".into(), serde_json::Value::Array(trace));
        obj.insert("chain_anchors".into(), serde_json::Value::Array(anchors));
        obj.insert(
            "chain_invariants".into(),
            serde_json::Value::Array(invariants),
        );
        obj.insert(
            "chain_branches".into(),
            serde_json::Value::Array(branches_summary),
        );
        obj.insert("current_method".into(), serde_json::json!(m_js));
    }
    Ok(Some(heap.from_json(&leaf)?))
}
