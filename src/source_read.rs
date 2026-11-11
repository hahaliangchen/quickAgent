//! Versioned, bounded source snapshots. Display pages never redefine the saved source.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::SystemTime,
};

pub const FILE_LIMIT: usize = 16 * 1024 * 1024;
const CACHE_LIMIT: usize = 64 * 1024 * 1024;
/// The largest page an AI receives, and the page it gets when it names none.
pub const PAGE_CHARS: u64 = 60_000;
const DEFAULT_PAGE_CHARS: u64 = 24_000;
/// Program code gets a read as a JS value, not as AI context, so one page
/// holds a whole ordinary file: every file within the default max_bytes fits.
/// 2026-11-11: six saved modules each paged 60,000 characters at a time.
pub const PROGRAM_PAGE_CHARS: u64 = 2_000_000;
/// Marks a read made by program code. Only the host sets it: every entry that
/// takes arguments from a caller removes it first (`unmark`).
pub const PROGRAM_READ: &str = "$program_read";

/// Drop the host's own marks from arguments a caller supplied.
pub fn unmark(args: &mut Value) {
    if let Some(object) = args.as_object_mut() {
        object.remove(PROGRAM_READ);
    }
}
/// Mark a source read as made by program code.
pub fn mark_program_read(name: &str, args: &mut Value) {
    if is_source_read(name)
        && let Some(object) = args.as_object_mut()
    {
        object.insert(PROGRAM_READ.into(), json!(true));
    }
}
/// The paging sentence of a source read's contract, as an AI calling the tool
/// reads it or as program code does.
pub fn page_note(program: bool) -> String {
    if program {
        format!(
            "In program code the content is a JS value, not AI context: without max_chars one call returns the whole file or requested range (complete:true) up to {PROGRAM_PAGE_CHARS} characters, so a file within max_bytes needs no paging loop."
        )
    } else {
        format!("max_chars defaults to {DEFAULT_PAGE_CHARS}, max {PAGE_CHARS}.")
    }
}
#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
}
fn stamp(meta: &fs::Metadata) -> Stamp {
    Stamp {
        size: meta.len(),
        modified: meta.modified().ok(),
        created: meta.created().ok(),
    }
}
pub struct Snapshot {
    pub content: String,
    pub hash: String,
    pub offsets: Vec<usize>,
    stamp: Stamp,
}
impl Snapshot {
    pub fn line_count(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }
    pub fn range(&self, start: usize, end: usize) -> io::Result<&str> {
        if start == 0 || end < start || end > self.line_count() {
            return Err(io::Error::other(format!(
                "source_range_out_of_bounds: requested {start}-{end}, total_lines={}",
                self.line_count()
            )));
        }
        Ok(&self.content[self.offsets[start - 1]..self.offsets[end]])
    }
}
pub fn line_offsets(content: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            offsets.push(index + 1);
        }
    }
    if offsets.last().copied() != Some(content.len()) {
        offsets.push(content.len());
    }
    offsets
}
struct Cache {
    entries: BTreeMap<PathBuf, (Arc<Snapshot>, u64)>,
    bytes: usize,
    tick: u64,
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(|| {
        Mutex::new(Cache {
            entries: BTreeMap::new(),
            bytes: 0,
            tick: 0,
        })
    })
}
fn cost(snapshot: &Snapshot) -> usize {
    snapshot.content.len() + snapshot.offsets.len() * std::mem::size_of::<usize>()
}
pub fn invalidate(path: &Path) {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut cache = cache().lock().unwrap_or_else(|error| error.into_inner());
    if let Some((old, _)) = cache.entries.remove(&path) {
        cache.bytes = cache.bytes.saturating_sub(cost(&old));
    }
}
pub fn clear() {
    let mut cache = cache().lock().unwrap_or_else(|error| error.into_inner());
    cache.entries.clear();
    cache.bytes = 0;
}
pub fn read(path: &Path, limit: usize) -> io::Result<Arc<Snapshot>> {
    let path = path.canonicalize()?;
    crate::database::check_source_access(&path)?;
    let limit = limit.min(FILE_LIMIT);
    let before = fs::metadata(&path)?;
    if !before.is_file() {
        return Err(io::Error::other("source path is not a regular file"));
    }
    if before.len() > limit as u64 {
        return Err(io::Error::other(format!(
            "source_file_too_large: {} bytes exceeds {limit}; use a bounded external reader for larger files",
            before.len()
        )));
    }
    {
        let mut cache = cache().lock().unwrap_or_else(|error| error.into_inner());
        cache.tick = cache.tick.wrapping_add(1);
        let tick = cache.tick;
        if let Some((snapshot, used)) = cache
            .entries
            .get_mut(&path)
            .filter(|(snapshot, _)| before.modified().is_ok() && snapshot.stamp == stamp(&before))
        {
            *used = tick;
            return Ok(snapshot.clone());
        }
    }
    for _ in 0..2 {
        let file = fs::File::open(&path)?;
        let before = file.metadata()?;
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(io::Error::other(format!(
                "source_file_too_large: exceeds {limit} bytes during read"
            )));
        }
        let after = fs::metadata(&path)?;
        if stamp(&before) != stamp(&after) {
            continue;
        }
        let content = String::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let snapshot = Arc::new(Snapshot {
            hash: crate::symbol_description::content_hash(content.as_bytes()),
            offsets: line_offsets(&content),
            content,
            stamp: stamp(&after),
        });
        let mut cache = cache().lock().unwrap_or_else(|error| error.into_inner());
        if let Some((old, _)) = cache.entries.remove(&path) {
            cache.bytes = cache.bytes.saturating_sub(cost(&old));
        }
        if cost(&snapshot) <= CACHE_LIMIT {
            while cache.bytes + cost(&snapshot) > CACHE_LIMIT || cache.entries.len() >= 128 {
                let Some(key) = cache
                    .entries
                    .iter()
                    .min_by_key(|(_, (_, used))| *used)
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                if let Some((old, _)) = cache.entries.remove(&key) {
                    cache.bytes = cache.bytes.saturating_sub(cost(&old));
                }
            }
            cache.tick = cache.tick.wrapping_add(1);
            let tick = cache.tick;
            cache.bytes += cost(&snapshot);
            cache.entries.insert(path.clone(), (snapshot.clone(), tick));
        }
        return Ok(snapshot);
    }
    Err(io::Error::other(
        "source_version_conflict: file changed while reading; retry the focused read",
    ))
}

pub fn indexed_hash(root: &Path, language: &str, file: &str) -> Option<String> {
    crate::database::init_db(root).ok()?.query_row("SELECT content_hash FROM symbol_index_files WHERE workspace_root=?1 AND language=?2 AND file_path=?3",rusqlite::params![root.to_string_lossy(),language,file],|row|row.get(0)).ok()
}
pub fn needs_index_refresh(root: &Path, language: &str, file: &str) -> bool {
    read(&root.join(file), FILE_LIMIT).is_ok_and(|snapshot| {
        indexed_hash(root, language, file).as_deref() != Some(snapshot.hash.as_str())
    })
}

/// Full-line display page, preserving CRLF and the final delimiter. The
/// callers retain the original requested bounds for source provenance.
pub fn page(result: &mut Value, args: &Value) -> io::Result<()> {
    let Some(content) = result["content"].as_str().map(str::to_owned) else {
        return Ok(());
    };
    let origin = result
        .get("start_line")
        .or_else(|| result.pointer("/symbol/start_line"))
        .and_then(Value::as_u64)
        .unwrap_or(1) as usize;
    let offsets = line_offsets(&content);
    let count = offsets.len().saturating_sub(1);
    let material_end = origin + count.saturating_sub(1);
    let from = if result.get("lines").is_none() {
        args["start_line"].as_u64().unwrap_or(origin as u64) as usize
    } else {
        origin
    };
    let until = if result.get("lines").is_none() {
        args["end_line"].as_u64().unwrap_or(material_end as u64) as usize
    } else {
        material_end
    };
    if count > 0 && (from < origin || from > material_end || until < from) {
        return Err(io::Error::other(format!(
            "source_range_out_of_bounds: material covers {origin}-{material_end}"
        )));
    }
    let until = until.min(material_end);
    let (default, most) = if args[PROGRAM_READ] == true {
        (PROGRAM_PAGE_CHARS, PROGRAM_PAGE_CHARS)
    } else {
        (DEFAULT_PAGE_CHARS, PAGE_CHARS)
    };
    let budget = args["max_chars"].as_u64().unwrap_or(default).clamp(1000, most) as usize;
    if args["start_line"].as_u64() == Some(0) || args["end_line"].as_u64() == Some(0) {
        return Err(io::Error::other("line numbers start at 1"));
    }
    let column = args["start_column"].as_u64().unwrap_or(1) as usize;
    if column == 0 {
        return Err(io::Error::other("start_column starts at 1"));
    }
    if count > 0 {
        let line = &content[offsets[from - origin]..offsets[from - origin + 1]];
        let line_chars = line.chars().count();
        if column > line_chars + 1 {
            return Err(io::Error::other("start_column is outside this line"));
        }
        if column > 1 || line_chars > budget {
            let fragment = line
                .chars()
                .skip(column - 1)
                .take(budget)
                .collect::<String>();
            let next_column = column + fragment.chars().count();
            let line_complete = next_column > line_chars;
            result["material_start_line"] = json!(origin);
            result["material_end_line"] = json!(material_end);
            result["content"] = json!(fragment);
            result["start_line"] = json!(from);
            result["end_line"] = json!(from);
            result["start_column"] = json!(column);
            result["partial_line"] = json!(true);
            result["returned_chars"] = json!(next_column - column);
            result["complete"] = json!(line_complete && from == until);
            result["next_start_line"] = if !line_complete {
                json!(from)
            } else if from < until {
                json!(from + 1)
            } else {
                Value::Null
            };
            result["next_column"] = if line_complete {
                Value::Null
            } else {
                json!(next_column)
            };
            result["source_format"] = json!({"byte_exact":true,"fragment":true,"column_unit":"Unicode characters, including line delimiters"});
            if result.get("lines").is_some() {
                result["lines"] = json!([]);
            }
            return Ok(());
        }
    }
    let mut end = from.saturating_sub(1);
    let mut chars = 0;
    if count > 0 {
        for line in from..=until {
            let part = &content[offsets[line - origin]..offsets[line - origin + 1]];
            let size = part.chars().count();
            if chars + size > budget {
                break;
            }
            chars += size;
            end = line;
        }
    }
    if count > 0 && end < from {
        return Err(io::Error::other(format!(
            "source_line_exceeds_page_budget: raise max_chars up to {most} or use another focused range"
        )));
    }
    let page = if count == 0 {
        String::new()
    } else {
        content[offsets[from - origin]..offsets[end - origin + 1]].to_owned()
    };
    result["material_start_line"] = json!(origin);
    result["material_end_line"] = json!(if count == 0 { 0 } else { material_end });
    result["start_line"] = json!(from);
    result["end_line"] = json!(end);
    result["complete"] = json!(count == 0 || end >= until);
    result["next_start_line"] = if end >= until || count == 0 {
        Value::Null
    } else {
        json!(end + 1)
    };
    result["returned_chars"] = json!(chars);
    result["partial_line"] = json!(false);
    result["start_column"] = json!(1);
    result["next_column"] = Value::Null;
    result["content"] = json!(page);
    if result.get("lines").is_some() {
        result["lines"] = json!(
            result["content"]
                .as_str()
                .unwrap_or("")
                .lines()
                .enumerate()
                .map(|(index, text)| json!({"line":from+index,"text":text}))
                .collect::<Vec<_>>()
        );
    }
    result["source_format"] = json!({"byte_exact":true,"newline":if content.contains("\r\n"){"CRLF"}else{"LF"},"ends_with_newline":content.ends_with('\n')});
    Ok(())
}

pub fn is_source_read(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "read_file_lines"
            | "read_ts_symbol"
            | "read_rust_symbol"
            | "read_go_symbol"
            | "read_python_symbol"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paged(content: &str, args: Value) -> Value {
        let mut result = json!({"content":content});
        page(&mut result, &args).unwrap();
        result
    }

    /// 2026-11-11: program code got an AI's 60,000-character page, and six
    /// saved modules each wrote their own paging loop to read one file.
    #[test]
    fn program_code_reads_a_whole_file_and_an_ai_keeps_its_page() {
        let content = "0123456789abcdefghi\n".repeat(10_000);
        let ai = paged(&content, json!({"path":"a.md","max_chars":400_000}));
        assert_eq!(ai["complete"], false);
        assert_eq!(ai["returned_chars"], 60_000);
        assert_eq!(ai["next_start_line"], 3001);

        let program = paged(&content, json!({"path":"a.md", PROGRAM_READ:true}));
        assert_eq!(program["complete"], true);
        assert_eq!(program["content"].as_str().unwrap(), content);
        assert!(program["next_start_line"].is_null());

        // A program that asks for pages still gets them.
        let asked = paged(&content, json!({"path":"a.md","max_chars":60_000, PROGRAM_READ:true}));
        assert_eq!((asked["complete"].clone(), asked["returned_chars"].clone()), (json!(false), json!(60_000)));

        // Past the program's own limit the same paging fields continue the read.
        let huge = "0123456789abcdefghi\n".repeat(150_000);
        let first = paged(&huge, json!({"path":"a.md", PROGRAM_READ:true}));
        assert_eq!(first["complete"], false);
        assert_eq!(first["returned_chars"], PROGRAM_PAGE_CHARS);
        assert_eq!(first["next_start_line"], 100_001);
    }

    #[test]
    fn only_the_host_marks_a_program_read() {
        let mut supplied = json!({"path":"a.md", PROGRAM_READ:true});
        unmark(&mut supplied);
        assert_eq!(supplied, json!({"path":"a.md"}));
        mark_program_read("search_text", &mut supplied);
        assert_eq!(supplied, json!({"path":"a.md"}), "only source reads are marked");
        mark_program_read("read_file", &mut supplied);
        assert_eq!(supplied[PROGRAM_READ], true);
    }
}
