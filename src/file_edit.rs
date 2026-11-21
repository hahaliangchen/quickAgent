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
    crate::database::check_source_access(&path)?;
    Ok(path)
}

pub fn snapshot(path: &Path) -> Result<Option<Vec<u8>>> {
    crate::database::check_source_access(path)?;
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn check_hash(original: Option<&[u8]>, expected: Option<&str>) -> Result<()> {
    if let Some(expected) = expected {
        let actual = original.map(crate::symbol_description::content_hash);
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
    crate::source_read::invalidate(path);
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
    if start == 0 || end < start || end > spans.len() {
        return Err(ToolError::InvalidLineRange { start, end });
    }
    let (begin, finish) = (spans[start - 1].0, spans[end - 1].1);
    let selected = &source[begin..finish];
    let selected_text = normalize(selected);
    if let Some(expected) = expected {
        let expected = normalize(expected);
        if expected.strip_suffix('\n').unwrap_or(&expected)
            != selected_text.strip_suffix('\n').unwrap_or(&selected_text)
        {
            return Err(ToolError::ExpectedTextMismatch);
        }
    }
    let ending = newline(if selected.contains('\n') {
        selected
    } else {
        source
    });
    let mut inserted = normalize(replacement).replace('\n', ending);
    if !inserted.is_empty()
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
            let start = start.max(1).saturating_sub(5).max(1);
            let end = end.max(start).saturating_add(5).min(lines.len());
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

/// How often and on which lines an ambiguous old_text occurs.
fn occurrences(source: &str, anchor: &str) -> String {
    let (mut lines, mut count, mut from) = (Vec::<usize>::new(), 0, 0);
    while let Some(found) = source[from..].find(anchor) {
        let at = from + found;
        count += 1;
        let line = source[..at].matches('\n').count() + 1;
        if lines.last() != Some(&line) {
            lines.push(line);
        }
        if count == 10 {
            break;
        }
        from = at + anchor.chars().next().map_or(1, char::len_utf8);
    }
    let list = lines.iter().map(usize::to_string).collect::<Vec<_>>().join(", ");
    let times = if count == 10 { "10 or more".to_owned() } else { count.to_string() };
    format!("it occurs {times} times, at {} {list}", if lines.len() == 1 { "line" } else { "lines" })
}

pub fn text_edits(source: &str, edits: &[crate::tools::TextEdit]) -> Result<String> {
    text_edits_with_ranges(source, edits).map(|(output, _)| output)
}

/// Apply text edits and return byte spans of their replacements in the updated file.
pub fn text_edits_with_ranges(source: &str, edits: &[crate::tools::TextEdit]) -> Result<(String, Vec<(usize, usize)>)> {
    text_edits_with_ranges_replace_all(source, edits, false)
}

/// Apply every matching anchor when requested; the default remains unique-anchor editing.
pub fn text_edits_with_ranges_replace_all(
    source: &str,
    edits: &[crate::tools::TextEdit],
    replace_all: bool,
) -> Result<(String, Vec<(usize, usize)>)> {
    if edits.is_empty() || edits.len() > 100 {
        return Err(ToolError::InvalidEdit(
            "provide between 1 and 100 edits".into(),
        ));
    }
    let ending = newline(source);
    let mut replacements = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(ToolError::InvalidEdit(format!(
                "edit {index}: old_text must be a nonempty unique anchor; insert by retaining the anchor in new_text"
            )));
        }
        // Model excerpts commonly use LF for a CRLF file. Match either the
        // literal bytes or that exact newline-normalized anchor, never regex.
        let normalized = normalize(&edit.old_text).replace('\n', ending);
        let anchor = if source.contains(&edit.old_text) {
            &edit.old_text
        } else {
            &normalized
        };
        let matches = source.match_indices(anchor).collect::<Vec<_>>();
        if matches.is_empty() {
            return Err(ToolError::InvalidEdit(match closest_lines(source, &edit.old_text) {
                Some(lines) => format!(
                    "edit {index}: old_text not found; no edits applied. The closest {lines}\nCopy old_text exactly from these lines, without the line numbers."
                ),
                None => format!(
                    "edit {index}: old_text not found, and no line of it appears in the file; no edits applied"
                ),
            }));
        }
        if !replace_all && matches.len() > 1 {
            return Err(ToolError::InvalidEdit(format!(
                "edit {index}: old_text is ambiguous: {}; supply more surrounding text; no edits applied",
                occurrences(source, anchor)
            )));
        }
        let replacement = normalize(&edit.new_text).replace('\n', ending);
        for (start, _) in matches {
            replacements.push((start, start + anchor.len(), replacement.clone()));
        }
    }
    replacements.sort_by_key(|edit| edit.0);
    if replacements.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(ToolError::InvalidEdit(
            "edit anchors overlap; no edits applied".into(),
        ));
    }
    let mut ranges = Vec::with_capacity(replacements.len());
    let mut delta: i64 = 0;
    for (begin, end, text) in &replacements {
        let final_begin = (*begin as i64 + delta).max(0) as usize;
        ranges.push((final_begin, final_begin + text.len()));
        delta += text.len() as i64 - (*end - *begin) as i64;
    }
    let mut output = source.to_owned();
    for (begin, end, text) in replacements.into_iter().rev() {
        output.replace_range(begin..end, &text);
    }
    Ok((output, ranges))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old_text: &str) -> crate::tools::TextEdit {
        crate::tools::TextEdit { old_text: old_text.into(), new_text: "changed".into() }
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
        assert!(unrelated.contains("no line of it appears in the file") && !unrelated.contains("|"), "{unrelated}");
    }

    #[test]
    fn an_ambiguous_anchor_says_where_it_occurs() {
        let message = error("x = 1;\ny = 2;\nx = 1;\n", "x = 1;");
        assert!(message.contains("it occurs 2 times, at lines 1, 3") && message.contains("supply more surrounding text"), "{message}");
        assert!(error("aa;\n", "a").contains("at line 1"));
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
            &[crate::tools::TextEdit { old_text: "two".into(), new_text: "TWO\nTWO-B".into() }],
        ).unwrap();
        assert_eq!(updated, "one\nTWO\nTWO-B\nthree\nfour\n");
        assert_eq!(preview_for_byte_ranges(&updated, &ranges).lines().next(), Some("L1|one"));
        assert!(preview_for_byte_ranges(&updated, &ranges).contains("L3|TWO-B"));
    }
    #[test]
    fn replace_all_updates_every_matching_anchor() {
        let edits = [crate::tools::TextEdit { old_text: "let value = 1;".into(), new_text: "let value = 2;".into() }];
        let (updated, ranges) = text_edits_with_ranges_replace_all(
            "let value = 1;\nlet value = 1;\n",
            &edits,
            true,
        ).unwrap();
        assert_eq!(updated, "let value = 2;\nlet value = 2;\n");
        assert_eq!(ranges.len(), 2);
    }
}
