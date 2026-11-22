//! High-entropy structural indexers and formatting helpers for oversized values
//! (`> 64KB`: symbol outlines, command error/tail summaries, array schema samples,
//! and line-numbered text views).

use serde_json::Value;

pub(super) fn format_large_source_index(
    label: &str,
    text: &str,
    path_hint: Option<&str>,
) -> String {
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
            symbols.push(format!("  L{}|{}", idx + 1, compact_single_line(t, 120)));
            if symbols.len() >= 80 {
                break;
            }
        }
    }
    let head = all_lines
        .iter()
        .take(15)
        .enumerate()
        .map(|(i, l)| format!("{}|{l}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let tail_start = total.saturating_sub(15);
    let tail = all_lines
        .iter()
        .skip(tail_start)
        .enumerate()
        .map(|(i, l)| format!("{}|{l}", tail_start + i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let title = path_hint.unwrap_or(label);
    let sym_block = if symbols.is_empty() {
        "  (no top-level symbol lines matched; use `.grep(pattern)` or `.slice(start, end)` on the handle)".into()
    } else {
        symbols.join("\n")
    };
    format!(
        "// [{label} LARGE INDEX ({title}, {total} lines, {chars} chars) — > 64KB]\n\
         // Page in exact lines on demand via handle methods: `.slice(start, end)`, `.slice([[s1, e1], [s2, e2]])`, `.grep(pattern, ctx)`, or `.outline()`:\n\
         // Symbols / Sections:\n{sym_block}\n\
         // Head (L1..L{}):\n{head}\n\
         // Tail (L{}..L{total}):\n{tail}",
        total.min(15),
        tail_start + 1
    )
}

pub(super) fn format_large_log_index(name: &str, exit: &Value, combined: &str) -> String {
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
            hits.push(format!("  L{}|{}", idx + 1, compact_single_line(line, 140)));
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
        .map(|(i, l)| format!("{}|{l}", tail_start + i + 1))
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

pub(super) fn format_large_array_index(name: &str, arr: &[Value], shape: &str) -> String {
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

pub(super) fn describe_item_shape(sample: &Value) -> String {
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

pub(super) fn add_line_numbers(text: &str, start_line: usize) -> String {
    text.lines()
        .enumerate()
        .map(|(idx, line)| format!("{}|{line}", start_line + idx))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::add_line_numbers;

    #[test]
    fn line_numbers_keep_separator_separate_from_source_indentation() {
        assert_eq!(
            add_line_numbers("fn main() {}\n    let x = 1;", 300),
            "300|fn main() {}\n301|    let x = 1;"
        );
    }
}

pub(super) fn format_comment_block(label: &str, body: &str) -> String {
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

pub(super) fn compact_single_line(text: &str, max_chars: usize) -> String {
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
