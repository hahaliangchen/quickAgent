//! UTF-8 edits with preconditions and byte-preserving replacements.
use crate::tools::{Result, ToolError};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

static WRITES: OnceLock<Mutex<std::collections::HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();

pub fn file_lock(path: &Path) -> Arc<Mutex<()>> {
    #[cfg(windows)]
    let key = PathBuf::from(path.to_string_lossy().to_lowercase());
    #[cfg(not(windows))]
    let key = path.to_path_buf();
    let mut locks = WRITES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) { return lock; }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub fn workspace_path(root: &Path, raw: &str) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let input = Path::new(raw);
    if !input.is_absolute()
        && input
            .components()
            .any(|part| matches!(part, Component::Prefix(_) | Component::RootDir))
    {
        return Err(ToolError::AbsolutePath(raw.into()));
    }
    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        root.join(input)
    };
    if candidate
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(ToolError::ParentTraversal(raw.into()));
    }
    let mut ancestor = candidate.as_path();
    while !ancestor.exists() {
        // A dangling symlink must not be treated as a new regular destination.
        if fs::symlink_metadata(ancestor).is_ok() {
            return Err(ToolError::WriteConflict(
                "dangling link in destination".into(),
            ));
        }
        ancestor = ancestor
            .parent()
            .ok_or_else(|| ToolError::OutsideWorkspace(raw.into()))?;
    }
    let resolved = ancestor.canonicalize()?;
    if !resolved.starts_with(&root) {
        return Err(ToolError::OutsideWorkspace(raw.into()));
    }
    let suffix = candidate
        .strip_prefix(ancestor)
        .map_err(|_| ToolError::OutsideWorkspace(raw.into()))?;
    // Joining an empty suffix adds a trailing separator to an existing file on
    // Windows, so it is then treated as a directory (ERROR_DIRECTORY / 267).
    let path = if suffix.as_os_str().is_empty() {
        resolved
    } else {
        resolved.join(suffix)
    };
    crate::context::database::check_source_access(&path)?;
    Ok(path)
}

pub fn snapshot(path: &Path) -> Result<Option<Vec<u8>>> {
    crate::context::database::check_source_access(path)?;
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn check_hash(original: Option<&[u8]>, expected: Option<&str>) -> Result<()> {
    if let Some(expected) = expected {
        let actual = original.map(crate::index::symbol_description::content_hash);
        if actual.as_deref() != Some(expected) {
            return Err(ToolError::WriteConflict(format!(
                "expected_code_hash={expected}, current_code_hash={}; retrieve current source before retrying",
                actual.as_deref().unwrap_or("missing")
            )));
        }
    }
    Ok(())
}

pub fn commit(path: &Path, original: Option<&[u8]>, content: &[u8]) -> Result<bool> {
    if snapshot(path)?.as_deref() != original {
        return Err(ToolError::WriteConflict(
            "file changed since it was read; no edit was applied".into(),
        ));
    }
    if original == Some(content) {
        return Ok(false);
    }
    if path.file_name().is_none() {
        return Err(ToolError::NotFile(path.display().to_string()));
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = path.with_file_name(format!(
        ".agent-write-{}-{stamp}-{}.tmp",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut created = false;
    let outcome = (|| -> Result<()> {
        let permissions = fs::metadata(path).ok().map(|meta| meta.permissions());
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        created = true;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        if let Some(permissions) = permissions {
            fs::set_permissions(&tmp, permissions)?;
        }
        // The per-file write lock covers direct MCP and Agent writes. Also
        // reject external changes observed before replacing the destination.
        if snapshot(path)?.as_deref() != original {
            return Err(ToolError::WriteConflict(
                "file changed before commit; no edit was applied".into(),
            ));
        }
        if original.is_none() {
            // Publishing a new file must not overwrite an external creator.
            publish_new(&tmp, path).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    ToolError::WriteConflict(
                        "destination was created before commit; no edit was applied".into(),
                    )
                } else {
                    error.into()
                }
            })?;
        } else {
            fs::rename(&tmp, path)?;
        }
        Ok(())
    })();
    if outcome.is_err() && created {
        #[cfg(windows)]
        if let Ok(meta) = fs::metadata(&tmp) {
            let mut permissions = meta.permissions();
            permissions.set_readonly(false);
            let _ = fs::set_permissions(&tmp, permissions);
        }
        let _ = fs::remove_file(&tmp);
    }
    outcome?;
    crate::index::source_read::invalidate(path);
    Ok(true)
}

#[cfg(windows)]
fn publish_new(tmp: &Path, path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let from = tmp
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let to = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // No REPLACE_EXISTING flag: an external creator must cause a conflict.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 8) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(not(windows))]
fn publish_new(tmp: &Path, path: &Path) -> std::io::Result<()> {
    fs::hard_link(tmp, path)?;
    if let Err(error) = fs::remove_file(tmp) {
        tracing::warn!(%error,"new file saved; temporary hard link could not be removed");
    }
    Ok(())
}

fn newline(source: &str) -> &'static str {
    match source.find('\n') {
        Some(index) if index > 0 && source.as_bytes()[index - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}
fn normalize(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}
pub fn line_replacement(
    source: &str,
    start: usize,
    end: usize,
    replacement: &str,
    expected: Option<&str>,
) -> Result<String> {
    let spans = source
        .split_inclusive('\n')
        .scan(0, |offset, line| {
            let begin = *offset;
            *offset += line.len();
            Some((begin, *offset))
        })
        .collect::<Vec<_>>();
    let pure_insert = start >= 1 && end == start - 1 && end <= spans.len();
    if !pure_insert && (start == 0 || end < start || end > spans.len()) {
        return Err(ToolError::InvalidLineRange { start, end });
    }
    let (begin, finish) = if pure_insert {
        let offset = if start <= spans.len() {
            spans[start - 1].0
        } else {
            source.len()
        };
        (offset, offset)
    } else {
        (spans[start - 1].0, spans[end - 1].1)
    };
    let selected = &source[begin..finish];
    let selected_text = normalize(selected);
    if let Some(expected) = expected {
        let expected = normalize(expected);
        let expected_trimmed = expected.strip_suffix('\n').unwrap_or(&expected);
        let selected_trimmed = selected_text.strip_suffix('\n').unwrap_or(&selected_text);
        if expected_trimmed != selected_trimmed {
            let detail = expected_mismatch_detail(source, start, end, expected_trimmed);
            return Err(ToolError::ExpectedTextMismatch(detail));
        }
    }
    let ending = newline(if selected.contains('\n') {
        selected
    } else {
        source
    });
    let mut inserted = normalize(replacement).replace('\n', ending);
    if pure_insert {
        if !inserted.is_empty() {
            if begin == source.len()
                && !source.is_empty()
                && !source.ends_with('\n')
                && !inserted.starts_with('\n')
                && !inserted.starts_with("\r\n")
            {
                inserted.insert_str(0, ending);
            }
            if !inserted.ends_with('\n') && (finish < source.len() || source.ends_with('\n')) {
                inserted.push_str(ending);
            }
        }
    } else if !inserted.is_empty()
        && !inserted.ends_with('\n')
        && (finish < source.len() || selected.ends_with('\n'))
    {
        inserted.push_str(ending);
    }
    let mut output = String::with_capacity(source.len() + inserted.len());
    output.push_str(&source[..begin]);
    output.push_str(&inserted);
    output.push_str(&source[finish..]);
    Ok(output)
}

/// Explain a `replace_range` `expected_old_text` mismatch with the actual lines
/// currently at `start..=end` and (when found) the exact shifted line range.
fn expected_mismatch_detail(
    source: &str,
    start: usize,
    end: usize,
    expected_trimmed: &str,
) -> String {
    let normalized = normalize(source);
    let source_lines: Vec<&str> = normalized
        .split_inclusive('\n')
        .map(|line| line.strip_suffix('\n').unwrap_or(line))
        .collect();
    let expected_lines: Vec<&str> = if expected_trimmed.is_empty() {
        vec![""]
    } else {
        expected_trimmed.split('\n').collect()
    };
    let mut matches = Vec::new();
    let window = expected_lines.len();
    if window > 0 && window <= source_lines.len() {
        for idx in 0..=source_lines.len() - window {
            if source_lines[idx..idx + window] == expected_lines[..] {
                matches.push((idx + 1, idx + window));
            }
        }
    }
    let mut out = format!(" ({start}-{end})");
    if matches.len() == 1 {
        let (hit_start, hit_end) = matches[0];
        out.push_str(&format!(
            "; exact expected_old_text is now at lines {hit_start}-{hit_end}"
        ));
    } else if matches.len() > 1 {
        let &(hit_start, hit_end) = matches
            .iter()
            .min_by_key(|(hit_start, _)| hit_start.abs_diff(start))
            .unwrap();
        out.push_str(&format!(
            "; nearest exact match is at lines {hit_start}-{hit_end} ({} matches total)",
            matches.len()
        ));
    }
    if end < start {
        out.push_str(&format!(
            ". Requested range is an empty insertion before line {start} (expected_old_text must be omitted or empty)."
        ));
        return out;
    }
    out.push_str(". Actual lines at requested range:\n");
    let max_lines = 12usize;
    let preview_end = end.min(start + max_lines - 1).min(source_lines.len());
    let mut chars = 0usize;
    for line_no in start..=preview_end {
        let line = source_lines[line_no - 1];
        let rendered = format!("{line_no}|{line}");
        if chars + rendered.len() > 800 && line_no > start {
            out.push_str("…\n");
            break;
        }
        chars += rendered.len() + 1;
        out.push_str(&rendered);
        out.push('\n');
    }
    if preview_end < end {
        out.push_str(&format!("… ({} more lines in selected range)\n", end - preview_end));
    }
    out.trim_end_matches('\n').to_owned()
}

/// Render compact, line-numbered context around changed regions in the updated file.
/// Ranges are 1-based inclusive line ranges and overlapping context windows merge.
pub fn preview_for_line_ranges(content: &str, ranges: &[(usize, usize)]) -> String {
    if ranges.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let mut windows = ranges
        .iter()
        .map(|&(start, end)| {
            let clamped = start.max(1).min(lines.len());
            let start = clamped.saturating_sub(5).max(1);
            let end = end.max(clamped).saturating_add(5).min(lines.len());
            (start, end)
        })
        .collect::<Vec<_>>();
    windows.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in windows {
        if let Some((_, previous_end)) = merged.last_mut() {
            if start <= *previous_end + 1 {
                *previous_end = (*previous_end).max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    let mut output = String::new();
    for (window_index, (start, end)) in merged.into_iter().enumerate() {
        if window_index > 0 {
            output.push_str("\n…\n");
        }
        for line_number in start..=end {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&format!("L{line_number}|{}", lines[line_number - 1]));
            if output.chars().count() >= 4000 {
                output = output.chars().take(3999).collect();
                output.push('…');
                return output;
            }
        }
    }
    output
}

fn line_number_at(content: &str, byte_offset: usize) -> usize {
    content.as_bytes()[..byte_offset.min(content.len())]
        .iter()
        .copied()
        .filter(|&byte| byte == b'\n')
        .count()
        + 1
}

/// Convert byte spans in the updated file into line ranges for `preview_for_line_ranges`.
pub fn preview_for_byte_ranges(content: &str, ranges: &[(usize, usize)]) -> String {
    let line_ranges = ranges
        .iter()
        .map(|&(start, end)| {
            let first = line_number_at(content, start);
            let last = line_number_at(content, end.saturating_sub(1).max(start));
            (first, last.max(first))
        })
        .collect::<Vec<_>>();
    preview_for_line_ranges(content, &line_ranges)
}

/// The file's lines from `first` (0-based), numbered as the file numbers them.
fn numbered(lines: &[&str], first: usize, count: usize) -> String {
    lines.iter().enumerate().skip(first).take(count).map(|(i, line)| {
        let mut text: String = line.chars().take(200).collect();
        if line.chars().count() > 200 {
            text.push('…');
        }
        format!("{}|{}", i + 1, text)
    }).collect::<Vec<_>>().join("\n")
}

/// Where a missing old_text most nearly is: the start line that most of its
/// lines agree on, ignoring indentation; for a single line with a small
/// difference, the line sharing the most words with it. None when nothing
/// in the file resembles it.
fn closest_lines(source: &str, old_text: &str) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let old: Vec<&str> = old_text.lines().collect();
    let mut at = std::collections::HashMap::<&str, Vec<usize>>::new();
    for (i, line) in lines.iter().enumerate() {
        at.entry(line.trim()).or_default().push(i);
    }
    let mut votes = std::collections::HashMap::<usize, usize>::new();
    for (j, line) in old.iter().enumerate() {
        // Braces and blank lines match everywhere and say nothing.
        if line.trim().len() < 4 {
            continue;
        }
        for &i in at.get(line.trim()).into_iter().flatten() {
            if i >= j {
                *votes.entry(i - j).or_default() += 1;
            }
        }
    }
    let words = |text: &str| -> std::collections::HashSet<String> {
        text.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|w| !w.is_empty()).map(str::to_owned).collect()
    };
    let start = votes.into_iter().max_by_key(|&(start, n)| (n, std::cmp::Reverse(start))).map(|(start, _)| start).or_else(|| {
        let (j, longest) = old.iter().enumerate().max_by_key(|(_, line)| line.trim().len())?;
        let wanted = words(longest);
        let (best, shared) = lines.iter().enumerate().map(|(i, line)| (i, words(line).intersection(&wanted).count()))
            .max_by_key(|&(i, n)| (n, std::cmp::Reverse(i)))?;
        (shared >= 2).then(|| best.saturating_sub(j))
    })?;
    let first = start.saturating_sub(1);
    let count = (old.len() + 2).min(30);
    Some(format!("lines in the file now are {}-{}:\n{}", first + 1, (first + count).min(lines.len()), numbered(&lines, first, count)))
}

fn select_anchor(source: &str, anchor: &str, hint: Option<usize>) -> std::result::Result<usize, String> {
    let (mut from, mut previous, mut line) = (0, 0, 1usize);
    let stride = anchor.chars().next().unwrap().len_utf8();
    let mut best = (usize::MAX, 0);
    let mut second = usize::MAX;
    let mut lines = Vec::new();
    let mut count = 0;
    while let Some(found) = source[from..].find(anchor) {
        let at = from + found;
        from = at + stride; // Count overlapping occurrences, too.
        line += source.as_bytes()[previous..at].iter().filter(|&&b| b == b'\n').count();
        previous = at;
        count += 1;
        if lines.len() < 10 && lines.last() != Some(&line) { lines.push(line); }
        let distance = hint.map_or(0, |hint| hint.abs_diff(line));
        if count == 1 || distance < best.0 {
            second = best.0;
            best = (distance, at);
        } else {
            second = second.min(distance);
        }
    }
    if count == 1
        || (hint.is_some_and(|hint| hint > 0)
            && ((best.0 == 0 && second > 0) || (best.0 <= 30 && second >= best.0 + 10)))
    {
        return Ok(best.1);
    }
    let list = lines.iter().map(usize::to_string).collect::<Vec<_>>().join(", ");
    let suffix = if count > 10 && lines.len() == 10 { ", ..." } else { "" };
    Err(format!("old_text is ambiguous: it occurs {count} times, at {} {list}{suffix}; supply more surrounding text or a 1-based line_hint near a clearly separated match; no edits applied", if lines.len() == 1 { "line" } else { "lines" }))
}

pub fn text_edits(source: &str, edits: &[crate::tools::TextEdit]) -> Result<String> {
    text_edits_with_ranges(source, edits).map(|(output, _)| output)
}

/// Apply text edits and return byte spans of their replacements in the updated file.
pub fn text_edits_with_ranges(source: &str, edits: &[crate::tools::TextEdit]) -> Result<(String, Vec<(usize, usize)>)> {
    text_edits_with_ranges_replace_all(source, edits, false)
}

fn find_trailing_ws_spans(source: &str, old_text: &str) -> Vec<(usize, usize)> {
    let norm = normalize(old_text);
    let ends_with_nl = norm.ends_with('\n');
    let body = if ends_with_nl { &norm[..norm.len() - 1] } else { &norm };
    if body.is_empty() {
        return Vec::new();
    }
    let needle: Vec<&str> = body
        .split('\n')
        .map(|l| l.trim_end_matches([' ', '\t']))
        .collect();
    if needle.iter().all(|l| l.is_empty()) {
        return Vec::new();
    }
    let mut lines: Vec<(usize, usize, usize, &str)> = Vec::new();
    let mut pos = 0usize;
    for chunk in source.split_inclusive('\n') {
        let end_with_eol = pos + chunk.len();
        let trimmed_eol = chunk.trim_end_matches(['\r', '\n']);
        let end_no_eol = pos + trimmed_eol.len();
        let trimmed_ws = trimmed_eol.trim_end_matches([' ', '\t']);
        lines.push((pos, end_no_eol, end_with_eol, trimmed_ws));
        pos = end_with_eol;
    }
    if lines.len() < needle.len() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    for start_idx in 0..=lines.len() - needle.len() {
        if needle
            .iter()
            .enumerate()
            .all(|(offset, expected)| lines[start_idx + offset].3 == *expected)
        {
            let byte_start = lines[start_idx].0;
            let last = lines[start_idx + needle.len() - 1];
            let byte_end = if ends_with_nl { last.2 } else { last.1 };
            matches.push((byte_start, byte_end));
        }
    }
    matches
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IndentMode {
    SameRelative,
    SpacesToTabs(usize),
    TabsToSpaces(usize),
}

#[derive(Clone, Debug)]
struct IndentMatch {
    span: (usize, usize),
    file_base: String,
    mode: IndentMode,
}

fn leading_ws(line: &str) -> &str {
    let end = line
        .bytes()
        .take_while(|&b| b == b' ' || b == b'\t')
        .count();
    &line[..end]
}

fn common_ws_prefix<'a>(a: &'a str, b: &str) -> &'a str {
    let n = a
        .bytes()
        .zip(b.bytes())
        .take_while(|(x, y)| x == y)
        .count();
    &a[..n]
}

fn common_leading_ws<'a>(lines: impl Iterator<Item = &'a str>) -> &'a str {
    let mut prefix: Option<&'a str> = None;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let ws = leading_ws(line);
        prefix = Some(match prefix {
            None => ws,
            Some(prev) => common_ws_prefix(prev, ws),
        });
    }
    prefix.unwrap_or("")
}

/// Find full-line windows whose non-empty code and relative line-to-line indentation
/// match `old_text` identically, differing only by a uniform leading indentation shift
/// (e.g. Go `\t\t\t` vs `\t\t`, or spaces vs tabs).
fn find_indent_shifted_spans(source: &str, old_text: &str) -> (String, Vec<IndentMatch>) {
    let norm = normalize(old_text);
    let ends_with_nl = norm.ends_with('\n');
    let body = if ends_with_nl { &norm[..norm.len() - 1] } else { &norm };
    if body.is_empty() {
        return (String::new(), Vec::new());
    }
    let old_lines: Vec<&str> = body
        .split('\n')
        .map(|l| l.trim_end_matches([' ', '\t']))
        .collect();
    if old_lines.iter().all(|l| l.is_empty()) {
        return (String::new(), Vec::new());
    }
    let old_base = common_leading_ws(old_lines.iter().copied());
    let old_parts: Vec<(bool, &str, &str)> = old_lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                (true, "", "")
            } else {
                let rest = &line[old_base.len()..];
                let rel = leading_ws(rest);
                (false, rel, &rest[rel.len()..])
            }
        })
        .collect();

    let mut src_lines: Vec<(usize, usize, usize, &str)> = Vec::new();
    let mut pos = 0usize;
    for chunk in source.split_inclusive('\n') {
        let end_with_eol = pos + chunk.len();
        let trimmed_eol = chunk.trim_end_matches(['\r', '\n']);
        let end_no_eol = pos + trimmed_eol.len();
        let trimmed_ws = trimmed_eol.trim_end_matches([' ', '\t']);
        src_lines.push((pos, end_no_eol, end_with_eol, trimmed_ws));
        pos = end_with_eol;
    }
    if src_lines.len() < old_parts.len() {
        return (old_base.to_owned(), Vec::new());
    }

    let mut matches = Vec::new();
    for start_idx in 0..=src_lines.len() - old_parts.len() {
        let window = &src_lines[start_idx..start_idx + old_parts.len()];
        // Fast check: code content (ignoring leading/trailing ws) and blank-line positions must match 100%.
        let code_matches = old_parts.iter().enumerate().all(|(k, &(blank, _, code))| {
            let line = window[k].3;
            if blank {
                line.is_empty()
            } else {
                !line.is_empty() && line.trim_start_matches([' ', '\t']) == code
            }
        });
        if !code_matches {
            continue;
        }
        let file_base = common_leading_ws(window.iter().map(|l| l.3));
        let rel_pairs: Vec<(&str, &str)> = old_parts
            .iter()
            .enumerate()
            .filter_map(|(k, &(blank, old_rel, _))| {
                if blank {
                    None
                } else {
                    let file_rel = leading_ws(&window[k].3[file_base.len()..]);
                    Some((old_rel, file_rel))
                }
            })
            .collect();

        let mode = if rel_pairs.iter().all(|(o, f)| o == f) {
            let all_rel_empty = rel_pairs.iter().all(|(o, _)| o.is_empty());
            if all_rel_empty
                && !old_base.is_empty()
                && !file_base.is_empty()
                && old_base.bytes().all(|b| b == b' ')
                && file_base.bytes().all(|b| b == b'\t')
                && old_base.len() % file_base.len() == 0
                && matches!(old_base.len() / file_base.len(), 2 | 4)
            {
                Some(IndentMode::SpacesToTabs(old_base.len() / file_base.len()))
            } else if all_rel_empty
                && !old_base.is_empty()
                && !file_base.is_empty()
                && old_base.bytes().all(|b| b == b'\t')
                && file_base.bytes().all(|b| b == b' ')
                && file_base.len() % old_base.len() == 0
                && matches!(file_base.len() / old_base.len(), 2 | 4)
            {
                Some(IndentMode::TabsToSpaces(file_base.len() / old_base.len()))
            } else {
                Some(IndentMode::SameRelative)
            }
        } else if let Some(width) = [4usize, 2].into_iter().find(|&w| {
            rel_pairs.iter().any(|(o, _)| !o.is_empty())
                && rel_pairs.iter().all(|(o, f)| {
                    o.bytes().all(|b| b == b' ')
                        && f.bytes().all(|b| b == b'\t')
                        && o.len() % w == 0
                        && f.len() == o.len() / w
                })
        }) {
            Some(IndentMode::SpacesToTabs(width))
        } else if let Some(width) = [4usize, 2].into_iter().find(|&w| {
            rel_pairs.iter().any(|(o, _)| !o.is_empty())
                && rel_pairs.iter().all(|(o, f)| {
                    o.bytes().all(|b| b == b'\t')
                        && f.bytes().all(|b| b == b' ')
                        && f.len() == o.len() * w
                })
        }) {
            Some(IndentMode::TabsToSpaces(width))
        } else {
            None
        };

        if let Some(mode) = mode {
            let byte_start = window[0].0;
            let last = window[window.len() - 1];
            let byte_end = if ends_with_nl { last.2 } else { last.1 };
            matches.push(IndentMatch {
                span: (byte_start, byte_end),
                file_base: file_base.to_owned(),
                mode,
            });
        }
    }
    (old_base.to_owned(), matches)
}

/// Uniformly shift `new_text` from `old_base` to `file_base` (and adapt relative
/// spaces/tabs when `mode` detected a spaces<->tabs mismatch). Returns `None` if
/// any non-empty line in `new_text` does not start with `old_base`, preventing
/// unintended indentation changes on out-of-block edits.
fn reindent_replacement(
    new_text: &str,
    old_base: &str,
    file_base: &str,
    mode: IndentMode,
    ending: &str,
) -> Option<String> {
    if new_text.is_empty() {
        return Some(String::new());
    }
    let norm = normalize(new_text);
    let ends_with_nl = norm.ends_with('\n');
    let body = if ends_with_nl { &norm[..norm.len() - 1] } else { &norm };
    let mut out_lines = Vec::new();
    for line in body.split('\n') {
        if line.trim().is_empty() {
            out_lines.push(String::new());
            continue;
        }
        let rest = line.strip_prefix(old_base)?;
        let rel = leading_ws(rest);
        let code = &rest[rel.len()..];
        let new_rel = match mode {
            IndentMode::SameRelative => rel.to_owned(),
            IndentMode::SpacesToTabs(w) => {
                if !rel.bytes().all(|b| b == b' ') || rel.len() % w != 0 {
                    return None;
                }
                "\t".repeat(rel.len() / w)
            }
            IndentMode::TabsToSpaces(w) => {
                if !rel.bytes().all(|b| b == b'\t') {
                    return None;
                }
                " ".repeat(rel.len() * w)
            }
        };
        out_lines.push(format!("{file_base}{new_rel}{code}"));
    }
    let mut result = out_lines.join(ending);
    if ends_with_nl {
        result.push_str(ending);
    }
    Some(result)
}

fn select_span(source: &str, spans: &[(usize, usize)], hint: Option<usize>) -> std::result::Result<(usize, usize), String> {
    let count = spans.len();
    let mut lines = Vec::new();
    let mut best = (usize::MAX, spans[0]);
    let mut second = usize::MAX;
    for &(start, end) in spans {
        let line = source[..start].bytes().filter(|&b| b == b'\n').count() + 1;
        if lines.len() < 10 {
            lines.push(line);
        }
        let distance = hint.map_or(0, |target| line.abs_diff(target));
        if distance < best.0 {
            second = best.0;
            best = (distance, (start, end));
        } else if distance < second {
            second = distance;
        }
    }
    if count == 1
        || (hint.is_some_and(|hint| hint > 0)
            && ((best.0 == 0 && second > 0) || (best.0 <= 30 && second >= best.0 + 10)))
    {
        return Ok(best.1);
    }
    let list = lines.iter().map(usize::to_string).collect::<Vec<_>>().join(", ");
    let suffix = if count > 10 && lines.len() == 10 { ", ..." } else { "" };
    Err(format!(
        "old_text is ambiguous: it occurs {count} times, at {} {list}{suffix}; supply more surrounding text or a 1-based line_hint near a clearly separated match; no edits applied",
        if lines.len() == 1 { "line" } else { "lines" }
    ))
}

fn adjust_ranges(ranges: &mut Vec<(usize, usize)>, start: usize, end: usize, new_len: usize) {
    let new_end = start + new_len;
    let delta = new_len as isize - (end - start) as isize;
    for (b, e) in ranges.iter_mut() {
        if *e <= start {
            continue;
        } else if *b >= end {
            *b = (*b as isize + delta).max(0) as usize;
            *e = (*e as isize + delta).max(0) as usize;
        } else {
            *b = (*b).min(start);
            *e = ((*e as isize + delta).max(new_end as isize)).max(0) as usize;
        }
    }
    ranges.push((start, new_end));
}

fn old_text_preview(raw: &str) -> String {
    const MAX_CHARS: usize = 80;
    let escaped = raw.replace('\r', "\\r").replace('\n', "\\n");
    let mut chars = escaped.chars();
    let clipped: String = chars.by_ref().take(MAX_CHARS).collect();
    let suffix = if chars.next().is_some() { "…" } else { "" };
    format!("{:?}{suffix}", clipped)
}

/// Apply text edits sequentially in memory (atomic all-or-nothing on error).
/// Sequential application supports both disjoint edits and adjacent/chained edits
/// without spurious anchor-overlap failures.
pub fn text_edits_with_ranges_replace_all(
    source: &str,
    edits: &[crate::tools::TextEdit],
    replace_all: bool,
) -> Result<(String, Vec<(usize, usize)>)> {
    if edits.is_empty() || edits.len() > 128 {
        return Err(ToolError::InvalidEdit(
            "provide between 1 and 128 edits".into(),
        ));
    }
    let ending = newline(source);
    let mut output = source.to_owned();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        let invalid = |message| ToolError::IndexedEdit { index, message };
        if edit.old_text.is_empty() {
            return Err(invalid("old_text must be a nonempty unique anchor; insert by retaining the anchor in new_text".into()));
        }
        // Model excerpts commonly use LF for a CRLF file. Match exact bytes first,
        // then newline-normalized bytes, then trailing-whitespace-normalized full lines,
        // then uniform leading-indentation-shifted full lines (auto-reindenting new_text).
        let normalized = normalize(&edit.old_text).replace('\n', ending);
        let replacement = normalize(&edit.new_text).replace('\n', ending);
        let old_preview = old_text_preview(&edit.old_text);
        let not_found_error = |current: &str| {
            invalid(match closest_lines(current, &edit.old_text) {
                Some(lines) => format!("old_text not found ({old_preview}); no edits applied. The closest {lines}\nCopy old_text exactly from these lines, without the line numbers."),
                None => format!("old_text not found ({old_preview}), and no line of it appears in the file; no edits applied"),
            })
        };
        let replacements: Vec<(usize, usize, String)> = if output.contains(&edit.old_text) {
            if replace_all || edit.replace_all {
                output
                    .match_indices(&edit.old_text)
                    .map(|(start, _)| (start, start + edit.old_text.len(), replacement.clone()))
                    .collect()
            } else {
                let start = select_anchor(&output, &edit.old_text, edit.line_hint).map_err(invalid)?;
                vec![(start, start + edit.old_text.len(), replacement.clone())]
            }
        } else if output.contains(&normalized) {
            if replace_all || edit.replace_all {
                output
                    .match_indices(&normalized)
                    .map(|(start, _)| (start, start + normalized.len(), replacement.clone()))
                    .collect()
            } else {
                let start = select_anchor(&output, &normalized, edit.line_hint).map_err(invalid)?;
                vec![(start, start + normalized.len(), replacement.clone())]
            }
        } else {
            let ws_matches = find_trailing_ws_spans(&output, &edit.old_text);
            if !ws_matches.is_empty() {
                if replace_all || edit.replace_all {
                    let mut filtered = Vec::new();
                    let mut cursor = 0usize;
                    for (s, e) in ws_matches {
                        if s >= cursor {
                            filtered.push((s, e, replacement.clone()));
                            cursor = e;
                        }
                    }
                    filtered
                } else {
                    let (s, e) = select_span(&output, &ws_matches, edit.line_hint).map_err(invalid)?;
                    vec![(s, e, replacement.clone())]
                }
            } else {
                let (old_base, indent_matches) = find_indent_shifted_spans(&output, &edit.old_text);
                if indent_matches.is_empty() {
                    return Err(not_found_error(&output));
                }
                let selected_matches: Vec<IndentMatch> = if replace_all || edit.replace_all {
                    let mut filtered = Vec::new();
                    let mut cursor = 0usize;
                    for m in indent_matches {
                        if m.span.0 >= cursor {
                            cursor = m.span.1;
                            filtered.push(m);
                        }
                    }
                    filtered
                } else {
                    let raw_spans: Vec<(usize, usize)> = indent_matches.iter().map(|m| m.span).collect();
                    let chosen = select_span(&output, &raw_spans, edit.line_hint).map_err(invalid)?;
                    let matched = indent_matches
                        .into_iter()
                        .find(|m| m.span == chosen)
                        .expect("selected span comes from indent_matches");
                    vec![matched]
                };
                let mut reindented = Vec::with_capacity(selected_matches.len());
                for m in selected_matches {
                    let Some(new_body) = reindent_replacement(
                        &edit.new_text,
                        &old_base,
                        &m.file_base,
                        m.mode,
                        ending,
                    ) else {
                        return Err(not_found_error(&output));
                    };
                    reindented.push((m.span.0, m.span.1, new_body));
                }
                reindented
            }
        };
        for (start, end, body) in replacements.into_iter().rev() {
            output.replace_range(start..end, &body);
            adjust_ranges(&mut ranges, start, end, body.len());
        }
    }
    ranges.sort_by_key(|r| r.0);
    Ok((output, ranges))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old_text: &str) -> crate::tools::TextEdit {
        crate::tools::TextEdit { old_text: old_text.into(), new_text: "changed".into(), line_hint: None, replace_all: false }
    }

    fn error(source: &str, old_text: &str) -> String {
        text_edits(source, &[edit(old_text)]).unwrap_err().to_string()
    }

    #[test]
    fn a_missing_anchor_shows_the_closest_lines_of_the_file() {
        let source = "fn a() {\n    let x = 1;\n    let y = 2;\n    return x + y;\n}\n\nfn b() {}\n";
        let message = error(source, "    let x = 1;\n    let y = 3;\n    return x + y;");
        assert!(message.contains("no edits applied") && message.contains("lines in the file now are 1-5"), "{message}");
        assert!(message.contains("3|    let y = 2;") && message.contains("Copy old_text exactly"), "{message}");
        let typo = error("const x = 2;\nconst y = 3;\n", "const x = 1;");
        assert!(typo.contains("1|const x = 2;"), "{typo}");
        let unrelated = error(source, "nothing like this");
        assert!(unrelated.contains("no line of it appears in the file") && unrelated.contains("nothing like this") && !unrelated.contains("|"), "{unrelated}");
        let quoted = error(source, "\"mentioned\"");
        assert!(quoted.contains("mentioned") && quoted.contains("no line of it appears in the file"), "{quoted}");
    }

    #[test]
    fn an_ambiguous_anchor_says_where_it_occurs() {
        let message = error("x = 1;\ny = 2;\nx = 1;\n", "x = 1;");
        assert!(message.contains("it occurs 2 times, at lines 1, 3") && message.contains("supply more surrounding text"), "{message}");
        assert!(error("aa;\n", "a").contains("at line 1"));
    }

    #[test]
    fn line_hint_requires_unique_nearby_match_with_clear_margin() {
        let source = format!("anchor\n{}anchor\n", "skip\n".repeat(39));
        let mut change = edit("anchor");
        for hint in [None, Some(0), Some(21), Some(24), Some(72)] {
            change.line_hint = hint;
            let err = text_edits(&source, std::slice::from_ref(&change)).unwrap_err();
            assert!(err.to_string().contains("lines 1, 41"), "{err}");
        }
        change.line_hint = Some(41);
        let modified = text_edits(&source, std::slice::from_ref(&change)).unwrap();
        assert!(modified.starts_with("anchor\n") && modified.ends_with("changed\n"));
        change.line_hint = Some(usize::MAX);
        assert_eq!(text_edits("anchor", &[change]).unwrap(), "changed");
        assert!(error("aaa", "aa").contains("ambiguous"));

        // Exact line_hint on a unique line disambiguates even when another match is <10 lines away.
        let nearby = "a\ntarget\nc\nd\ne\nf\ng\nh\ntarget\nz\n";
        let mut exact = edit("target");
        exact.line_hint = Some(2);
        let updated = text_edits(nearby, &[exact]).unwrap();
        assert_eq!(updated, "a\nchanged\nc\nd\ne\nf\ng\nh\ntarget\nz\n");

        // Two occurrences on the exact same line remain ambiguous even with exact line_hint.
        let same_line = "target and target\n";
        let mut dup_line = edit("target");
        dup_line.line_hint = Some(1);
        assert!(text_edits(same_line, &[dup_line]).unwrap_err().to_string().contains("ambiguous"));
    }

    #[test]
    fn changed_preview_merges_hunks_and_keeps_line_numbers() {
        let source = (1..=20).map(|line| format!("line {line}")).collect::<Vec<_>>().join("\n");
        let preview = preview_for_line_ranges(&source, &[(2, 2), (7, 8)]);
        assert!(preview.contains("L1|line 1"));
        assert!(preview.contains("L13|line 13"));
        assert!(!preview.contains("L14|line 14"));
    }

    #[test]
    fn changed_preview_has_a_hard_character_limit() {
        let source = (1..=100).map(|line| format!("{line}: {}", "x".repeat(100))).collect::<Vec<_>>().join("\n");
        let preview = preview_for_line_ranges(&source, &[(1, 100)]);
        assert!(preview.chars().count() <= 4000, "preview too long: {}", preview.chars().count());
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn text_edit_ranges_point_into_the_updated_file() {
        let (updated, ranges) = text_edits_with_ranges(
            "one\ntwo\nthree\nfour\n",
            &[crate::tools::TextEdit { old_text: "two".into(), new_text: "TWO\nTWO-B".into(), line_hint: None, replace_all: false }],
        ).unwrap();
        assert_eq!(updated, "one\nTWO\nTWO-B\nthree\nfour\n");
        assert_eq!(preview_for_byte_ranges(&updated, &ranges).lines().next(), Some("L1|one"));
        assert!(preview_for_byte_ranges(&updated, &ranges).contains("L3|TWO-B"));
    }
    #[test]
    fn replace_all_updates_every_matching_anchor() {
        let edits = [crate::tools::TextEdit { old_text: "let value = 1;".into(), new_text: "let value = 2;".into(), line_hint: None, replace_all: false }];
        let (updated, ranges) = text_edits_with_ranges_replace_all(
            "let value = 1;\nlet value = 1;\n",
            &edits,
            true,
        ).unwrap();
        assert_eq!(updated, "let value = 2;\nlet value = 2;\n");
        assert_eq!(ranges.len(), 2);
    }

    #[test]
    fn per_edit_replace_all_works_when_top_level_flag_is_false() {
        let edits = [
            crate::tools::TextEdit { old_text: "foo".into(), new_text: "bar".into(), line_hint: None, replace_all: true },
            crate::tools::TextEdit { old_text: "only_once".into(), new_text: "ONCE".into(), line_hint: None, replace_all: false },
        ];
        let (updated, ranges) = text_edits_with_ranges_replace_all(
            "foo\nonly_once\nfoo\n",
            &edits,
            false,
        ).unwrap();
        assert_eq!(updated, "bar\nONCE\nbar\n");
        assert_eq!(ranges.len(), 3);
    }

    #[test]
    fn line_replacement_mismatch_reports_actual_and_shifted_lines() {
        let source = "alpha\nbeta\ngamma\ndelta\n";
        let shifted = line_replacement(source, 3, 3, "BETA", Some("beta"))
            .unwrap_err()
            .to_string();
        assert!(
            shifted.contains("expected_old_text did not match the selected line range (3-3)"),
            "{shifted}"
        );
        assert!(
            shifted.contains("exact expected_old_text is now at lines 2-2"),
            "{shifted}"
        );
        assert!(shifted.contains("3|gamma"), "{shifted}");

        let missing = line_replacement(source, 2, 3, "X", Some("not here"))
            .unwrap_err()
            .to_string();
        assert!(missing.contains("2|beta") && missing.contains("3|gamma"), "{missing}");
        assert!(!missing.contains("exact expected_old_text is now at"), "{missing}");
    }

    #[test]
    fn sequential_edits_allow_adjacent_shared_anchors_and_chained_changes() {
        let source = "fn a() {\n    let x = 1;\n}\n";
        let edits = [
            crate::tools::TextEdit {
                old_text: "    let x = 1;\n}".into(),
                new_text: "    let x = 2;\n    let y = 3;\n}".into(),
                line_hint: None,
                replace_all: false,
            },
            crate::tools::TextEdit {
                old_text: "    let y = 3;\n}".into(),
                new_text: "    let y = 4;\n    let z = 5;\n}".into(),
                line_hint: None,
                replace_all: false,
            },
        ];
        let (updated, ranges) = text_edits_with_ranges(source, &edits).unwrap();
        assert_eq!(updated, "fn a() {\n    let x = 2;\n    let y = 4;\n    let z = 5;\n}\n");
        let preview = preview_for_byte_ranges(&updated, &ranges);
        assert!(preview.contains("L2|    let x = 2;"), "{preview}");
        assert!(preview.contains("L4|    let z = 5;"), "{preview}");
    }

    #[test]
    fn trailing_whitespace_fallback_matches_unique_lines_without_masking_ambiguity() {
        let source = "fn run() {   \n    let a = 1;  \n}\n";
        let edits = [crate::tools::TextEdit {
            old_text: "fn run() {\n    let a = 1;\n}".into(),
            new_text: "fn run() {\n    let a = 2;\n}".into(),
            line_hint: None,
            replace_all: false,
        }];
        let updated = text_edits(source, &edits).unwrap();
        assert_eq!(updated, "fn run() {\n    let a = 2;\n}\n");

        let dup = "let x = 1;  \nlet x = 1;\t\n";
        let err = text_edits(dup, &[edit("let x = 1;")]).unwrap_err().to_string();
        assert!(err.contains("ambiguous"), "{err}");
    }

    #[test]
    fn line_replacement_supports_pure_insert_when_end_equals_start_minus_one() {
        let source = "alpha\nbeta\ngamma\n";
        // Insert before line 2 (start=2, end=1)
        assert_eq!(
            line_replacement(source, 2, 1, "inserted", None).unwrap(),
            "alpha\ninserted\nbeta\ngamma\n"
        );
        // Prepend before line 1 (start=1, end=0)
        assert_eq!(
            line_replacement(source, 1, 0, "header\n", None).unwrap(),
            "header\nalpha\nbeta\ngamma\n"
        );
        // Append after last line when file ends with newline (start=4, end=3)
        assert_eq!(
            line_replacement(source, 4, 3, "delta", None).unwrap(),
            "alpha\nbeta\ngamma\ndelta\n"
        );
        // Append after last line when file has no trailing newline
        assert_eq!(
            line_replacement("alpha\nbeta", 3, 2, "gamma", None).unwrap(),
            "alpha\nbeta\ngamma"
        );
        // Pure insert into empty file (start=1, end=0)
        assert_eq!(
            line_replacement("", 1, 0, "first\n", None).unwrap(),
            "first\n"
        );
        // CRLF preservation
        assert_eq!(
            line_replacement("a\r\nb\r\n", 2, 1, "x\ny", None).unwrap(),
            "a\r\nx\r\ny\r\nb\r\n"
        );
        // expected_old_text: empty matches, non-empty fails with clear message
        assert_eq!(
            line_replacement(source, 2, 1, "ok", Some("")).unwrap(),
            "alpha\nok\nbeta\ngamma\n"
        );
        let mismatch = line_replacement(source, 2, 1, "ok", Some("beta"))
            .unwrap_err()
            .to_string();
        assert!(mismatch.contains("empty insertion before line 2"), "{mismatch}");

        // Inverted ranges with end < start - 1 or out-of-bounds remain invalid
        for (start, end) in [(0, 0), (3, 1), (2, 0), (5, 4)] {
            let err = line_replacement(source, start, end, "x", None).unwrap_err();
            assert!(
                matches!(err, ToolError::InvalidLineRange { .. }),
                "expected InvalidLineRange for ({start}, {end}), got {err}"
            );
        }
        assert!(matches!(
            line_replacement("", 2, 1, "x", None).unwrap_err(),
            ToolError::InvalidLineRange { start: 2, end: 1 }
        ));
    }

    #[test]
    fn indent_shifted_fallback_matches_go_tab_shift_and_reindents_new_text() {
        let source = concat!(
            "func run() {\n",
            "\tfor index := range records {\n",
            "\t\trecords[index].ChunkOrder = order\n",
            "\t\textra[\"page_start\"] = *records[index].PageStart\n",
            "\t\textra[\"page_end\"] = *records[index].PageEnd\n",
            "\t}\n",
            "}\n",
        );
        // Model miscounted 3 leading tabs instead of 2 on both old_text and new_text.
        let edits = [crate::tools::TextEdit {
            old_text: "\t\t\textra[\"page_start\"] = *records[index].PageStart\n\t\t\textra[\"page_end\"] = *records[index].PageEnd".into(),
            new_text: concat!(
                "\t\t\tif records[index].PageStart != nil {\n",
                "\t\t\t\textra[\"page_start\"] = *records[index].PageStart\n",
                "\t\t\t}\n",
                "\t\t\tif records[index].PageEnd != nil {\n",
                "\t\t\t\textra[\"page_end\"] = *records[index].PageEnd\n",
                "\t\t\t}",
            )
            .into(),
            line_hint: None,
            replace_all: false,
        }];
        let updated = text_edits(source, &edits).unwrap();
        let expected = concat!(
            "func run() {\n",
            "\tfor index := range records {\n",
            "\t\trecords[index].ChunkOrder = order\n",
            "\t\tif records[index].PageStart != nil {\n",
            "\t\t\textra[\"page_start\"] = *records[index].PageStart\n",
            "\t\t}\n",
            "\t\tif records[index].PageEnd != nil {\n",
            "\t\t\textra[\"page_end\"] = *records[index].PageEnd\n",
            "\t\t}\n",
            "\t}\n",
            "}\n",
        );
        assert_eq!(updated, expected);
    }

    #[test]
    fn indent_shifted_fallback_converts_spaces_to_tabs_and_preserves_safety_guards() {
        let go_source = "func f() {\n\tfor i := 0; i < n; i++ {\n\t\tif ok {\n\t\t\tdoWork()\n\t\t}\n\t}\n}\n";
        // Model wrote 8/12 spaces instead of 2/3 tabs.
        let space_edit = [crate::tools::TextEdit {
            old_text: "        if ok {\n            doWork()\n        }".into(),
            new_text: "        if ok {\n            doWork()\n            logDone()\n        }".into(),
            line_hint: None,
            replace_all: false,
        }];
        let updated = text_edits(go_source, &space_edit).unwrap();
        assert_eq!(
            updated,
            "func f() {\n\tfor i := 0; i < n; i++ {\n\t\tif ok {\n\t\t\tdoWork()\n\t\t\tlogDone()\n\t\t}\n\t}\n}\n"
        );

        // Safety 1: Ambiguous shifted matches still fail unless disambiguated by line_hint.
        let dup_source = "fn f() {\n\ta()\n\tb()\n\ta()\n\tb()\n}\n";
        let dup_edit = crate::tools::TextEdit {
            old_text: "\t\ta()\n\t\tb()".into(),
            new_text: "\t\tc()".into(),
            line_hint: None,
            replace_all: false,
        };
        assert!(
            text_edits(dup_source, std::slice::from_ref(&dup_edit))
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        let mut hinted = dup_edit;
        hinted.line_hint = Some(4);
        assert_eq!(
            text_edits(dup_source, &[hinted]).unwrap(),
            "fn f() {\n\ta()\n\tb()\n\tc()\n}\n"
        );

        // Safety 2: Refuse if new_text un-indents below old_base (e.g. Python out-of-block edit).
        let py_source = "def f():\n    if cond:\n        x = 1\n        y = 2\n";
        let unsafe_py = [crate::tools::TextEdit {
            old_text: "            x = 1\n            y = 2".into(),
            new_text: "    return x + y".into(),
            line_hint: None,
            replace_all: false,
        }];
        assert!(
            text_edits(py_source, &unsafe_py)
                .unwrap_err()
                .to_string()
                .contains("old_text not found")
        );

        // Safety 3: Refuse if relative indentation between lines differs.
        let bad_rel = [crate::tools::TextEdit {
            old_text: "    if cond:\n    x = 1".into(),
            new_text: "    if cond:\n    x = 2".into(),
            line_hint: None,
            replace_all: false,
        }];
        assert!(
            text_edits(py_source, &bad_rel)
                .unwrap_err()
                .to_string()
                .contains("old_text not found")
        );
    }
}
