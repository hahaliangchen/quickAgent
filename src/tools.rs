use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::go_index::{
    self, IndexGoWorkspaceRequest, ListGoSymbolsRequest, ReadGoSymbolRequest,
    SearchGoSymbolsRequest,
};
use crate::memory::{
    self, ListArchitectureMemoryRequest, ListSymbolBusinessContextRequest, ListWorkMemoryRequest,
    RecordArchitectureMemoryRequest, RecordSymbolBusinessContextRequest, RecordWorkMemoryRequest,
    SearchArchitectureMemoryRequest, SearchSymbolBusinessContextRequest, SearchWorkMemoryRequest,
};
use crate::python_index::{
    self, IndexPythonWorkspaceRequest, ListPythonSymbolsRequest, ReadPythonSymbolRequest,
    SearchPythonSymbolsRequest,
};
use crate::rust_index::{
    self, IndexRustWorkspaceRequest, ListRustSymbolsRequest, ReadRustSymbolRequest,
    SearchRustSymbolsRequest,
};
use crate::ts_index::{
    self, IndexTsWorkspaceRequest, ListTsSymbolsRequest, ReadTsSymbolRequest,
    SearchTsSymbolsRequest,
};

const DEFAULT_MAX_READ_BYTES: u64 = 1024 * 1024;
const DEFAULT_CHUNK_BYTES: usize = 64 * 1024;
const MAX_CHUNK_BYTES: usize = 512 * 1024;
const DEFAULT_MAX_MATCHES: usize = 100;
const MAX_CONTEXT_LINES: usize = 20;
const DEFAULT_WRITE_LIMIT_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_SEARCH_FILE_LIMIT_BYTES: u64 = 2 * 1024 * 1024;
/// Inline byte budget for `search_text` matches/counts so responses stay well below
/// the 96 KiB direct-tool delivery soft cap and remain in-context.
const MAX_SEARCH_RESPONSE_BYTES: usize = 48 * 1024;
/// A matched line longer than this comes back as an excerpt around the match.
const MATCH_TEXT_CHARS: usize = 400;
/// How much of a long line before the match its excerpt keeps.
const MATCH_CONTEXT_CHARS: usize = 100;
const NOISE_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".swarm",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".turbo",
    ".venv",
    "venv",
    "__pycache__",
];

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("absolute paths are not accepted: {0}")]
    AbsolutePath(String),
    #[error("parent traversal is not accepted: {0}")]
    ParentTraversal(String),
    #[error("path does not exist: {0}")]
    NotFound(String),
    #[error("path is not a file: {0}")]
    NotFile(String),
    #[error("path is not a directory: {0}")]
    NotDirectory(String),
    #[error("workspace_root must be an absolute directory path: {0}")]
    WorkspaceRootMustBeAbsolute(String),
    #[error("workspace_root is required for this tool")]
    WorkspaceRootRequired,
    #[error("file is too large: {actual} bytes exceeds limit {limit} bytes")]
    FileTooLarge { actual: u64, limit: u64 },
    #[error("content is too large: {actual} bytes exceeds limit {limit} bytes")]
    ContentTooLarge { actual: usize, limit: usize },
    #[error("invalid line range: start_line={start}, end_line={end}")]
    InvalidLineRange { start: usize, end: usize },
    #[error("expected_old_text did not match the selected line range")]
    ExpectedTextMismatch,
    #[error("write_conflict: {0}")]
    WriteConflict(String),
    #[error("write_precondition_required: {0}")]
    WritePreconditionRequired(String),
    #[error("path is outside the selected workspace: {0}")]
    OutsideWorkspace(String),
    #[error("invalid_edit: {0}")]
    InvalidEdit(String),
    #[error("invalid search pattern: {0}")]
    InvalidSearchPattern(String),
    #[error("invalid parameter: {0}")]
    InvalidParameter(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("utf-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

pub type Result<T> = std::result::Result<T, ToolError>;

#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceInfo {
    pub workspace_root: String,
    pub platform: String,
    pub allowed_scope: String,
    pub default_ignored_dirs: Vec<&'static str>,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceInfoRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GitStatusRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default = "default_true")]
    pub include_untracked: bool,
    #[serde(default)]
    pub max_entries: Option<usize>,
}

impl Default for GitStatusRequest {
    fn default() -> Self {
        Self { workspace_root: None, paths: Vec::new(), include_untracked: true, max_entries: None }
    }
}

#[derive(Debug, Serialize)]
pub struct GitStatusResponse {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub clean: bool,
    pub staged: Vec<String>,
    pub modified: Vec<String>,
    pub untracked: Vec<String>,
    pub total_entries: usize,
    pub truncated: bool,
}

#[derive(Debug, Deserialize)]
pub struct GitDiffRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub rev_range: Option<String>,
    #[serde(default)]
    pub staged: bool,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub stat_only: bool,
    #[serde(default = "default_git_unified")]
    pub unified: usize,
    #[serde(default)]
    pub max_lines: Option<usize>,
    #[serde(default)]
    pub max_chars: Option<usize>,
}

impl Default for GitDiffRequest {
    fn default() -> Self {
        Self { workspace_root: None, rev_range: None, staged: false, paths: Vec::new(), stat_only: false, unified: 3, max_lines: None, max_chars: None }
    }
}

#[derive(Debug, Serialize)]
pub struct GitDiffResponse {
    pub diff: String,
    pub files: Vec<String>,
    pub truncated: bool,
    pub stat_only: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct GitLogRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub rev: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GitCommit {
    pub commit: String,
    pub short_commit: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

#[derive(Debug, Serialize)]
pub struct GitLogResponse {
    pub commits: Vec<GitCommit>,
    pub truncated: bool,
}

#[derive(Debug, Deserialize)]
pub struct ListDirRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default = "default_dot")]
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub max_depth: Option<usize>,
    #[serde(default)]
    pub respect_gitignore: bool,
    /// Only entries whose path matches one of these globs, as in `rg -g`:
    /// `*.rs` matches at any depth, `src/**/*.ts` from the workspace root,
    /// `|` separates alternatives outside `{...}`, and a leading `!` excludes.
    #[serde(default, alias = "globs", deserialize_with = "one_or_many")]
    pub glob: Vec<String>,
    /// Exclude files or directories matching these patterns (`|` separates
    /// alternatives outside `{...}`).
    #[serde(default, alias = "excludes", alias = "ignore", deserialize_with = "one_or_many")]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub kind: Option<EntryKind>,
    /// Keep entries modified at or after this time (RFC3339, `YYYY-MM-DD`,
    /// epoch seconds/ms, or relative duration like `24h`, `7d`).
    #[serde(default)]
    pub modified_after: Option<String>,
    /// Keep entries modified at or before this time (same formats as `modified_after`).
    #[serde(default)]
    pub modified_before: Option<String>,
    #[serde(default)]
    pub sort_by: ListSort,
    #[serde(default, alias = "limit", alias = "head_limit")]
    pub max_entries: Option<usize>,
}

impl Default for ListDirRequest {
    fn default() -> Self {
        Self {
            workspace_root: None,
            path: default_dot(),
            recursive: false,
            max_depth: None,
            respect_gitignore: false,
            glob: Vec::new(),
            exclude: Vec::new(),
            kind: None,
            modified_after: None,
            modified_before: None,
            sort_by: ListSort::default(),
            max_entries: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ListDirResponse {
    pub root: String,
    pub entries: Vec<DirEntryInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unexpanded_dirs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct DirEntryInfo {
    pub path: String,
    pub kind: EntryKind,
    pub size_bytes: Option<u64>,
    pub modified_at: Option<String>,
    pub modified_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListSort {
    #[default]
    Path,
    ModifiedDesc,
    ModifiedAsc,
    SizeDesc,
    SizeAsc,
}

#[derive(Debug, Deserialize)]
pub struct ReadFileRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    #[serde(default = "default_max_read_bytes")]
    pub max_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct ReadFileResponse {
    pub code_hash: String,
    pub path: String,
    pub bytes: usize,
    pub content: String,
    pub total_lines: usize,
}

fn default_tail_end_line() -> isize {
    -1
}

#[derive(Debug, Deserialize)]
pub struct ReadFileLinesRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    pub start_line: isize,
    #[serde(default = "default_tail_end_line")]
    pub end_line: isize,
}

#[derive(Debug, Serialize)]
pub struct ReadFileLinesResponse {
    pub code_hash: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub lines: Vec<LineContent>,
    pub content: String,
    pub total_lines: usize,
    pub requested_end_line: isize,
    pub end_clamped_to_eof: bool,
}

#[derive(Debug, Serialize)]
pub struct LineContent {
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct ReadFileChunkRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    #[serde(default)]
    pub byte_offset: u64,
    #[serde(default)]
    pub start_line: Option<usize>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub max_lines: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ChunkLine {
    pub line: Option<usize>,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct ReadFileChunkResponse {
    pub path: String,
    pub file_bytes: u64,
    pub start_byte: u64,
    pub end_byte: u64,
    pub next_byte_offset: Option<u64>,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub next_line: Option<usize>,
    pub eof: bool,
    pub line_truncated: bool,
    pub bytes: usize,
    pub line_count: usize,
    pub content: String,
    pub lines: Vec<ChunkLine>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SearchTextRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub query: String,
    /// None: matched as a regular expression when it is written as one
    /// (`a|b`, `name\(`, `^fn`, `.*`), otherwise literally.
    #[serde(default)]
    pub regex: Option<bool>,
    #[serde(default = "default_dot")]
    pub path: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub case_sensitive: bool,
    /// None: files that .gitignore or .ignore excludes are skipped, as rg
    /// does, unless the path searched is itself one of them.
    #[serde(default)]
    pub respect_gitignore: Option<bool>,
    #[serde(default = "default_max_matches", alias = "limit", alias = "head_limit")]
    pub max_matches: usize,
    /// Optional per-file match cap in `content` mode so one noisy file cannot
    /// exhaust `max_matches`.
    #[serde(default)]
    pub max_matches_per_file: Option<usize>,
    /// What comes back: matching lines (`content`), the files that match
    /// (`files`), or each file's count of matching lines (`count`).
    #[serde(default)]
    pub output_mode: SearchOutput,
    /// Only files whose path matches one of these globs, as in `rg -g`:
    /// `*.rs` matches at any depth, `src/**/*.ts` from the workspace root,
    /// `|` separates alternatives outside `{...}`, and a leading `!` excludes.
    #[serde(default, alias = "globs", deserialize_with = "one_or_many")]
    pub glob: Vec<String>,
    /// Exclude files or directories matching these patterns (`|` separates
    /// alternatives outside `{...}`).
    #[serde(default, alias = "excludes", alias = "ignore", deserialize_with = "one_or_many")]
    pub exclude: Vec<String>,
    /// Lines before and after each match, in content mode.
    #[serde(default, alias = "context")]
    pub context_lines: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchOutput {
    #[default]
    Content,
    #[serde(alias = "files_with_matches", alias = "files_only")]
    Files,
    Count,
}

fn one_or_many<'de, D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany { One(String), Many(Vec<String>) }
    Ok(match Option::<OneOrMany>::deserialize(deserializer)? {
        None => Vec::new(),
        Some(OneOrMany::One(one)) => vec![one],
        Some(OneOrMany::Many(many)) => many,
    })
}

/// A query nobody asked to treat as a regular expression is treated as one
/// when it is written as one and compiles: an alternation, an escape, an
/// anchor or `.*`. 2026-11-13: of 25 searches, 3 written as `a|b|c` or
/// `model_round\(` without regex:true matched literally, found nothing, and
/// the AI turned to grep in a shell. A pattern that matches the empty
/// string (`a || b` read as regex) stays literal, as does `arr[0]`.
fn written_as_regex(query: &str) -> bool {
    // A separator-only query such as " | " is ordinary text, not an alternation.
    if query.trim() == "|" {
        return false;
    }
    let marked = query.contains('|')
        || query.contains('\\')
        || query.contains(".*")
        || query.contains(".+")
        || query.contains("(?")
        || query.starts_with('^')
        || (query.ends_with('$') && !query.ends_with("\\$"));
    marked && regex::Regex::new(query).is_ok_and(|re| !re.is_match(""))
}

#[derive(Debug, Serialize)]
pub struct SearchTextResponse {
    pub query: String,
    pub match_mode: &'static str,
    pub output_mode: SearchOutput,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<&'static str>,
    pub index_used: bool,
    pub index_matches: usize,
    pub text_scan_used: bool,
    pub matches: Vec<TextMatch>,
    /// Files mode: the matching files, in walk order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    /// Count mode: matching lines per file, and their total.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<Vec<FileCount>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct FileCount {
    pub path: String,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct TextMatch {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub text: String,
    /// The whole line's length in characters, given when `text` is an excerpt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_chars: Option<usize>,
    /// With context_lines: the lines just before and after, nearest last
    /// and first, each cut like `text` when long.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct WriteFileRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    pub content: String,
    #[serde(default = "default_true")]
    pub create_parent_dirs: bool,
    pub expected_code_hash: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WriteFileResponse {
    pub path: String,
    pub bytes_written: usize,
    pub go_reindexed: bool,
    pub changed: bool,
    pub created: bool,
    pub code_hash: String,
    pub index_refresh: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeletePathRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    /// Directories need this: a plain delete removes files only.
    #[serde(default)]
    pub recursive: bool,
}

#[derive(Debug, Serialize)]
pub struct DeletePathResponse {
    pub path: String,
    pub deleted: bool,
    /// "file" or "directory".
    pub kind: String,
}

#[derive(Debug, Deserialize)]
pub struct RenamePathRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    pub to: String,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default = "default_true")]
    pub create_parent_dirs: bool,
}

#[derive(Debug, Serialize)]
pub struct RenamePathResponse {
    pub path: String,
    pub to: String,
    pub overwritten: bool,
    pub index_refresh: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct CreateDirRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    #[serde(default = "default_true")]
    pub recursive: bool,
}

#[derive(Debug, Serialize)]
pub struct CreateDirResponse {
    pub path: String,
    pub created: bool,
}
#[derive(Debug, Deserialize)]
pub struct ReplaceRangeRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub replacement: String,
    pub expected_old_text: Option<String>,
    pub expected_code_hash: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReplaceRangeResponse {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub bytes_written: usize,
    pub go_reindexed: bool,
    pub changed: bool,
    pub code_hash: String,
    pub index_refresh: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TextEdit {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Deserialize)]
pub struct EditFileRequest {
    #[serde(default)]
    pub workspace_root: Option<String>,
    pub path: String,
    pub edits: Vec<TextEdit>,
    #[serde(default)]
    pub replace_all: bool,
    pub expected_code_hash: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GoIndexResult {
    pub index_path: String,
    pub files_indexed: usize,
    pub symbols_indexed: usize,
    pub generated_at_unix: u64,
}

#[derive(Debug, Serialize)]
pub struct PythonIndexResult {
    pub index_path: String,
    pub files_indexed: usize,
    pub symbols_indexed: usize,
    pub generated_at_unix: u64,
}

#[derive(Debug, Serialize)]
pub struct RustIndexResult {
    pub index_path: String,
    pub files_indexed: usize,
    pub symbols_indexed: usize,
    pub generated_at_unix: u64,
}

#[derive(Debug, Serialize)]
pub struct TsIndexResult {
    pub index_path: String,
    pub files_indexed: usize,
    pub symbols_indexed: usize,
    pub generated_at_unix: u64,
}

include!("parts/tools_workspace.inc.rs");

fn sanitize_relative_path(raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(ToolError::AbsolutePath(raw.to_string()));
    }

    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => clean.push(value),
            Component::ParentDir => return Err(ToolError::ParentTraversal(raw.to_string())),
            Component::Prefix(_) | Component::RootDir => {
                return Err(ToolError::AbsolutePath(raw.to_string()));
            }
        }
    }
    Ok(clean)
}

struct GitOutput {
    stdout: String,
}

fn validate_git_arg(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.starts_with('-') || value.contains('\0') {
        return Err(ToolError::InvalidParameter(format!("{label} is invalid")));
    }
    Ok(())
}

fn git_paths(paths: &[String]) -> Result<Vec<String>> {
    paths.iter().map(|path| {
        let clean = sanitize_relative_path(path)?;
        if clean.as_os_str().is_empty() {
            return Err(ToolError::InvalidParameter("git path must not be empty".to_string()));
        }
        Ok(clean.to_string_lossy().replace('\\', "/"))
    }).collect()
}

fn run_git(root: &Path, args: &[&str]) -> Result<GitOutput> {
    let output = Command::new("git")
        .args(["-c", "core.quotepath=off", "-c", "color.ui=never", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(ToolError::from)?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() {
        return Err(ToolError::InvalidParameter(format!("git {} failed: {}", args.first().copied().unwrap_or("command"), stderr)));
    }
    Ok(GitOutput { stdout })
}

fn git_text(root: &Path, args: &[&str]) -> Result<String> {
    Ok(run_git(root, args)?.stdout.trim().to_owned())
}

fn validate_line_range(start: usize, end: usize) -> Result<()> {
    if start == 0 || end == 0 || start > end {
        Err(ToolError::InvalidLineRange { start, end })
    } else {
        Ok(())
    }
}

fn resolve_line_bounds(
    total_lines: usize,
    start_line: isize,
    end_line: isize,
) -> Result<Option<(usize, usize, bool)>> {
    if start_line == 0 || end_line == 0 {
        return Err(ToolError::InvalidParameter(
            "start_line and end_line must be non-zero (1-based positive or negative from EOF)"
                .to_string(),
        ));
    }
    if total_lines == 0 {
        if start_line < 0 || (start_line == 1 && (end_line > 0 || end_line == -1)) {
            return Ok(None);
        }
        return Err(ToolError::InvalidEdit(format!(
            "source_range_out_of_bounds: start_line={start_line}, total_lines=0; no source returned"
        )));
    }
    let start = if start_line > 0 {
        start_line as usize
    } else {
        total_lines.saturating_sub(start_line.unsigned_abs()) + 1
    };
    let (end_raw, from_eof) = if end_line > 0 {
        (end_line as usize, false)
    } else {
        let back = end_line.unsigned_abs();
        if back > total_lines {
            return Err(ToolError::InvalidParameter(format!(
                "invalid line range: start_line={start_line}, end_line={end_line} before start of {total_lines}-line file"
            )));
        }
        (total_lines - back + 1, true)
    };
    if start > end_raw {
        return Err(ToolError::InvalidParameter(format!(
            "invalid line range: start_line={start_line} resolves after end_line={end_line}"
        )));
    }
    if start > total_lines {
        return Err(ToolError::InvalidEdit(format!(
            "source_range_out_of_bounds: start_line={start_line}, total_lines={total_lines}; no source returned"
        )));
    }
    let end = end_raw.min(total_lines);
    let clamped = !from_eof && end < end_raw;
    Ok(Some((start, end, clamped)))
}

fn fnv1a_update(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    hash
}

fn read_large_file_lines(
    path: &Path,
    rel_path: &str,
    start_line: isize,
    end_line: isize,
) -> Result<ReadFileLinesResponse> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut hash = 0xcbf29ce484222325u64;
    let mut total_lines: usize = 0;

    let tail_cap = (start_line < 0).then(|| start_line.unsigned_abs());
    let pos_start = (start_line > 0).then(|| start_line as usize);
    let pos_end = (end_line > 0).then(|| end_line as usize);
    let mut tail_ring: VecDeque<(usize, String)> = VecDeque::new();
    let mut forward_kept: Vec<(usize, String)> = Vec::new();
    let mut kept_bytes: usize = 0;

    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        hash = fnv1a_update(hash, &buf);
        total_lines += 1;
        let line_no = total_lines;

        if let Some(cap) = tail_cap {
            let text = String::from_utf8(std::mem::take(&mut buf))?;
            if tail_ring.len() == cap
                && let Some((_, dropped)) = tail_ring.pop_front()
            {
                kept_bytes = kept_bytes.saturating_sub(dropped.len());
            }
            kept_bytes += text.len();
            if kept_bytes > crate::source_read::FILE_LIMIT {
                return Err(ToolError::ContentTooLarge {
                    actual: kept_bytes,
                    limit: crate::source_read::FILE_LIMIT,
                });
            }
            tail_ring.push_back((line_no, text));
        } else if let Some(s) = pos_start {
            let in_window = line_no >= s && pos_end.is_none_or(|e| line_no <= e);
            if in_window {
                let text = String::from_utf8(std::mem::take(&mut buf))?;
                kept_bytes += text.len();
                if kept_bytes > crate::source_read::FILE_LIMIT {
                    return Err(ToolError::ContentTooLarge {
                        actual: kept_bytes,
                        limit: crate::source_read::FILE_LIMIT,
                    });
                }
                forward_kept.push((line_no, text));
            }
        }
    }

    let code_hash = format!("fnv1a64:{hash:016x}");
    let Some((resolved_start, resolved_end, end_clamped_to_eof)) =
        resolve_line_bounds(total_lines, start_line, end_line)?
    else {
        return Ok(ReadFileLinesResponse {
            code_hash,
            path: rel_path.to_string(),
            start_line: 0,
            end_line: 0,
            lines: Vec::new(),
            content: String::new(),
            total_lines: 0,
            requested_end_line: end_line,
            end_clamped_to_eof: false,
        });
    };

    let selected: Vec<(usize, String)> = if tail_cap.is_some() {
        tail_ring
            .into_iter()
            .filter(|(line_no, _)| *line_no >= resolved_start && *line_no <= resolved_end)
            .collect()
    } else {
        forward_kept
            .into_iter()
            .filter(|(line_no, _)| *line_no >= resolved_start && *line_no <= resolved_end)
            .collect()
    };

    let mut content = String::new();
    let mut lines = Vec::with_capacity(selected.len());
    for (line_no, raw) in selected {
        content.push_str(&raw);
        let stripped = raw.trim_end_matches(['\n', '\r']).to_string();
        lines.push(LineContent {
            line: line_no,
            text: stripped,
        });
    }

    Ok(ReadFileLinesResponse {
        code_hash,
        path: rel_path.to_string(),
        start_line: resolved_start,
        end_line: resolved_end,
        lines,
        content,
        total_lines,
        requested_end_line: end_line,
        end_clamped_to_eof,
    })
}

fn stream_file_chunk(
    path: &Path,
    rel_path: &str,
    byte_offset: u64,
    start_line: Option<usize>,
    max_bytes: usize,
    max_lines: Option<usize>,
) -> Result<ReadFileChunkResponse> {
    let mut file = fs::File::open(path)?;
    let file_bytes = file.metadata()?.len();
    if byte_offset > file_bytes {
        return Err(ToolError::InvalidParameter(format!(
            "byte_offset {byte_offset} exceeds file length {file_bytes}"
        )));
    }
    if byte_offset == file_bytes {
        return Ok(ReadFileChunkResponse {
            path: rel_path.to_string(),
            file_bytes,
            start_byte: byte_offset,
            end_byte: byte_offset,
            next_byte_offset: None,
            start_line: None,
            end_line: None,
            next_line: None,
            eof: true,
            line_truncated: false,
            bytes: 0,
            line_count: 0,
            content: String::new(),
            lines: Vec::new(),
        });
    }

    let max_bytes = max_bytes.clamp(4, MAX_CHUNK_BYTES);
    file.seek(SeekFrom::Start(byte_offset))?;
    let mut reader = BufReader::new(file);
    let mut raw_line = Vec::new();
    let mut content = String::new();
    let mut lines = Vec::new();
    let mut consumed: u64 = 0;
    let mut current_line = start_line.or_else(|| (byte_offset == 0).then_some(1));
    let mut line_truncated = false;

    loop {
        if max_lines.is_some_and(|cap| lines.len() >= cap) {
            break;
        }
        raw_line.clear();
        let n = reader.read_until(b'\n', &mut raw_line)?;
        if n == 0 {
            break;
        }
        if !lines.is_empty() && content.len() + raw_line.len() > max_bytes {
            break;
        }
        if raw_line.len() > max_bytes {
            let prefix_bytes = &raw_line[..max_bytes];
            let valid_len = match std::str::from_utf8(prefix_bytes) {
                Ok(_) => prefix_bytes.len(),
                Err(err) => err.valid_up_to(),
            };
            if valid_len == 0 {
                return Err(ToolError::InvalidParameter(
                    "invalid UTF-8 sequence at chunk boundary".to_string(),
                ));
            }
            let prefix = std::str::from_utf8(&raw_line[..valid_len])
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            content.push_str(prefix);
            lines.push(ChunkLine {
                line: current_line,
                text: prefix.trim_end_matches('\r').to_string(),
            });
            consumed += prefix.len() as u64;
            line_truncated = true;
            break;
        }
        let text = String::from_utf8(std::mem::take(&mut raw_line))?;
        consumed += text.len() as u64;
        content.push_str(&text);
        lines.push(ChunkLine {
            line: current_line,
            text: text.trim_end_matches(['\n', '\r']).to_string(),
        });
        current_line = current_line.map(|line| line + 1);
    }

    let end_byte = byte_offset + consumed;
    let eof = end_byte >= file_bytes;
    let next_byte_offset = (!eof).then_some(end_byte);
    let next_line = if eof { None } else { current_line };
    let start_line_out = lines.first().and_then(|line| line.line);
    let end_line_out = lines.last().and_then(|line| line.line);
    let line_count = lines.len();
    let bytes = content.len();

    Ok(ReadFileChunkResponse {
        path: rel_path.to_string(),
        file_bytes,
        start_byte: byte_offset,
        end_byte,
        next_byte_offset,
        start_line: start_line_out,
        end_line: end_line_out,
        next_line,
        eof,
        line_truncated,
        bytes,
        line_count,
        content,
        lines,
    })
}

fn check_write_size(size: usize) -> Result<()> {
    if size > DEFAULT_WRITE_LIMIT_BYTES {
        return Err(ToolError::ContentTooLarge {
            actual: size,
            limit: DEFAULT_WRITE_LIMIT_BYTES,
        });
    }
    Ok(())
}

pub(crate) fn refresh_written_index(root: &Path, path: &Path, changed: bool) -> serde_json::Value {
    use serde_json::json;
    if !changed {
        return json!({"status":"unchanged"});
    }
    let (language, table, result) = match path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => (
            "rust",
            "rust_symbols",
            rust_index::maybe_reindex_after_write(root, path)
                .map(|value| value.is_some())
                .map_err(|error| error.to_string()),
        ),
        Some("go") => (
            "go",
            "go_symbols",
            go_index::maybe_reindex_after_write(root, path)
                .map(|value| value.is_some())
                .map_err(|error| error.to_string()),
        ),
        Some("py") => (
            "python",
            "python_symbols",
            python_index::maybe_reindex_after_write(root, path)
                .map(|value| value.is_some())
                .map_err(|error| error.to_string()),
        ),
        Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs") => (
            "ts",
            "ts_symbols",
            ts_index::maybe_reindex_after_write(root, path)
                .map(|value| value.is_some())
                .map_err(|error| error.to_string()),
        ),
        _ => return json!({"status":"not_applicable"}),
    };
    match result {
        Ok(true) => {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let file_status=crate::database::init_db(root).ok().and_then(|conn|conn.query_row(
                "SELECT status,error FROM symbol_index_files WHERE workspace_root=?1 AND language=?2 AND file_path=?3",
                rusqlite::params![root.to_string_lossy(),language,relative],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).ok());
            match file_status {
                Some((status, error)) => {
                    json!({"status":if status=="indexed" {"updated"} else {"incomplete"},"language":language,"scope":"changed_file","file_status":status,"error":error,"index":crate::symbol_index_state::health(root,language,table).ok()})
                }
                None => {
                    json!({"status":"excluded","language":language,"scope":"changed_file","guidance":"File saved but not included by the index ignore rules."})
                }
            }
        }
        Ok(false) => json!({"status":"not_initialized","language":language}),
        Err(error) => {
            tracing::warn!(%language,%error,"file saved but index refresh failed");
            json!({"status":"failed","language":language,"error":error,"file_saved":true})
        }
    }
}

fn entry_kind(metadata: &fs::Metadata) -> EntryKind {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

fn default_dot() -> String {
    ".".to_string()
}

fn default_true() -> bool {
    true
}

fn default_git_unified() -> usize {
    3
}

fn default_max_read_bytes() -> u64 {
    DEFAULT_MAX_READ_BYTES
}

fn default_max_matches() -> usize {
    DEFAULT_MAX_MATCHES
}

fn split_pipe_patterns(patterns: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in patterns {
        let mut start = 0usize;
        let mut brace_depth = 0usize;
        let mut escaped = false;
        for (idx, ch) in raw.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '{' => brace_depth += 1,
                '}' => brace_depth = brace_depth.saturating_sub(1),
                '|' if brace_depth == 0 => {
                    let part = raw[start..idx].trim();
                    if !part.is_empty() {
                        out.push(part.to_owned());
                    }
                    start = idx + 1;
                }
                _ => {}
            }
        }
        let tail = raw[start..].trim();
        if !tail.is_empty() {
            out.push(tail.to_owned());
        }
    }
    out
}

fn allowed_noise_dirs(globs: &[String]) -> std::collections::HashSet<&'static str> {
    let mut allowed = std::collections::HashSet::new();
    for trimmed in split_pipe_patterns(globs) {
        if trimmed.is_empty() || trimmed.starts_with('!') {
            continue;
        }
        for &noise in NOISE_DIRS {
            if trimmed.split('/').any(|seg| seg == noise) {
                allowed.insert(noise);
            }
        }
    }
    allowed
}

fn format_modified(metadata: &fs::Metadata) -> (Option<String>, Option<i64>) {
    let Ok(time) = metadata.modified() else {
        return (None, None);
    };
    let dt: chrono::DateTime<chrono::Utc> = time.into();
    (
        Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        Some(dt.timestamp_millis()),
    )
}

fn parse_relative_duration_ms(raw: &str) -> Option<i64> {
    let body = raw.strip_prefix('-').unwrap_or(raw);
    if body.len() < 2 {
        return None;
    }
    let (digits, unit) = body.split_at(body.len() - 1);
    let value: i64 = digits.parse().ok()?;
    if value < 0 {
        return None;
    }
    let multiplier = match unit {
        "s" => 1_000i64,
        "m" => 60 * 1_000i64,
        "h" => 60 * 60 * 1_000i64,
        "d" => 24 * 60 * 60 * 1_000i64,
        "w" => 7 * 24 * 60 * 60 * 1_000i64,
        _ => return None,
    };
    value.checked_mul(multiplier)
}

fn parse_timestamp_ms(raw: &str, inclusive_end: bool) -> Result<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ToolError::InvalidParameter("timestamp cannot be empty".into()));
    }
    if let Ok(num) = trimmed.parse::<i64>() {
        return if num.abs() >= 100_000_000_000 {
            Ok(num)
        } else {
            num.checked_mul(1_000)
                .ok_or_else(|| ToolError::InvalidParameter(format!("timestamp out of range: {raw}")))
        };
    }
    if let Some(delta_ms) = parse_relative_duration_ms(trimmed) {
        return Ok(chrono::Utc::now().timestamp_millis() - delta_ms);
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.timestamp_millis());
    }
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Ok(ndt.and_utc().timestamp_millis());
        }
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let ndt = if inclusive_end {
            date.and_hms_milli_opt(23, 59, 59, 999)
        } else {
            date.and_hms_opt(0, 0, 0)
        }
        .ok_or_else(|| ToolError::InvalidParameter(format!("invalid date: {raw}")))?;
        return Ok(ndt.and_utc().timestamp_millis());
    }
    Err(ToolError::InvalidParameter(format!(
        "invalid timestamp {raw:?}; use RFC3339, YYYY-MM-DD, epoch seconds/ms, or e.g. 24h / 7d"
    )))
}

fn map_python_index_error(error: python_index::PythonIndexError) -> ToolError {
    ToolError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        error.to_string(),
    ))
}

fn map_go_index_error(error: go_index::GoIndexError) -> ToolError {
    ToolError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        error.to_string(),
    ))
}

fn map_memory_error(error: memory::MemoryError) -> ToolError {
    ToolError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        error.to_string(),
    ))
}

struct DirEntryFilter<'a> {
    workspace: &'a Workspace,
    kind: Option<EntryKind>,
    globs: Option<&'a CompiledGlobs>,
    modified_after: Option<i64>,
    modified_before: Option<i64>,
}

/// Applies caller filters to a path and records matching metadata in `entries`.
fn push_matching_dir_entry(
    filter: &DirEntryFilter<'_>,
    path: &Path,
    entries: &mut Vec<DirEntryInfo>,
) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    let kind = entry_kind(&metadata);
    if filter.kind.is_some_and(|want| want != kind) {
        return Ok(());
    }
    if let Some(globs) = filter.globs
        && !globs.matches(path, metadata.is_dir())
    {
        return Ok(());
    }
    let (modified_at, modified_ms) = format_modified(&metadata);
    if let Some(after) = filter.modified_after
        && !modified_ms.is_some_and(|ms| ms >= after)
    {
        return Ok(());
    }
    if let Some(before) = filter.modified_before
        && !modified_ms.is_some_and(|ms| ms <= before)
    {
        return Ok(());
    }
    entries.push(DirEntryInfo {
        path: filter.workspace.relative_display(path)?,
        kind,
        size_bytes: metadata.is_file().then_some(metadata.len()),
        modified_at,
        modified_ms,
    });
    Ok(())
}

/// Returns `true` when `entry` is a nested default noise directory whose subtree should stay unexpanded.
fn is_skipped_noise_dir(
    entry: &ignore::DirEntry,
    allowed_noise: &std::collections::HashSet<&'static str>,
) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_some_and(|ft| ft.is_dir()) {
        return false;
    }
    entry
        .file_name()
        .to_str()
        .is_some_and(|name| NOISE_DIRS.contains(&name) && !allowed_noise.contains(name))
}

fn list_dir_hints(unexpanded_dirs: &[String]) -> Vec<String> {
    if unexpanded_dirs.is_empty() {
        return Vec::new();
    }
    let preview = unexpanded_dirs
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let extra = unexpanded_dirs.len().saturating_sub(5);
    let suffix = if extra > 0 {
        format!(" (+{extra} more in unexpanded_dirs)")
    } else {
        String::new()
    };
    vec![format!(
        "Default noise/dependency directories were not traversed inside: {preview}{suffix} (directory entries remain visible when matched). Do not inspect inside these directories unless the user explicitly specified them or the task genuinely requires it (if needed, pass the directory path or name it in glob)."
    )]
}

#[derive(Clone)]
struct ScanOptions {
    mode: SearchOutput,
    respect_gitignore: bool,
    /// Matches in content mode, files otherwise.
    limit: usize,
    max_matches_per_file: Option<usize>,
    context_lines: usize,
    globs: Option<CompiledGlobs>,
    allowed_noise: std::collections::HashSet<&'static str>,
}

#[derive(Default)]
struct ScanFound {
    matches: Vec<TextMatch>,
    /// Files and counts modes: every matching file with its matching lines.
    counts: Vec<FileCount>,
    truncated: bool,
    skipped_noise: bool,
}

#[derive(Clone)]
struct CompiledGlobs {
    workspace_root: PathBuf,
    include: Option<ignore::overrides::Override>,
    exclude: Option<ignore::overrides::Override>,
}

impl CompiledGlobs {
    fn is_excluded(&self, path: &Path, is_dir: bool) -> bool {
        let Some(exclude) = &self.exclude else {
            return false;
        };
        if exclude.matched(path, is_dir).is_whitelist() {
            return true;
        }
        let mut cursor = path.parent();
        while let Some(dir) = cursor {
            if dir == self.workspace_root || !dir.starts_with(&self.workspace_root) {
                break;
            }
            if exclude.matched(dir, true).is_whitelist() {
                return true;
            }
            cursor = dir.parent();
        }
        false
    }

    fn matches(&self, path: &Path, is_dir: bool) -> bool {
        if self.is_excluded(path, is_dir) {
            return false;
        }
        match &self.include {
            Some(include) => include.matched(path, is_dir).is_whitelist(),
            None => true,
        }
    }
}

/// Globs as `rg -g` reads them, rooted at the workspace so `src/**/*.ts`
/// means the same from any search path, with `|` splitting outside `{...}`
/// and explicit `exclude` patterns taking strict priority over `glob`.
fn search_globs(
    root: &Path,
    globs: &[String],
    excludes: &[String],
) -> Result<Option<CompiledGlobs>> {
    let mut include_patterns = Vec::new();
    let mut exclude_patterns = Vec::new();
    for pattern in split_pipe_patterns(globs) {
        if let Some(neg) = pattern.strip_prefix('!') {
            let neg = neg.trim();
            if !neg.is_empty() {
                exclude_patterns.push(neg.to_owned());
            }
        } else {
            include_patterns.push(pattern);
        }
    }
    for pattern in split_pipe_patterns(excludes) {
        let pat = pattern.strip_prefix('!').unwrap_or(&pattern).trim();
        if !pat.is_empty() {
            exclude_patterns.push(pat.to_owned());
        }
    }
    if include_patterns.is_empty() && exclude_patterns.is_empty() {
        return Ok(None);
    }
    let build_override = |patterns: &[String]| -> Result<Option<ignore::overrides::Override>> {
        if patterns.is_empty() {
            return Ok(None);
        }
        let mut builder = ignore::overrides::OverrideBuilder::new(root);
        for pat in patterns {
            builder
                .add(pat)
                .map_err(|error| ToolError::InvalidSearchPattern(format!("glob {pat}: {error}")))?;
        }
        Ok(Some(
            builder
                .build()
                .map_err(|error| ToolError::InvalidSearchPattern(error.to_string()))?,
        ))
    };
    Ok(Some(CompiledGlobs {
        workspace_root: root.to_path_buf(),
        include: build_override(&include_patterns)?,
        exclude: build_override(&exclude_patterns)?,
    }))
}

/// Whether a path being searched is itself excluded by the workspace's
/// top-level .gitignore or .ignore, alone or through a parent folder.
fn ignored_target(workspace_root: &Path, target: &Path) -> bool {
    if target == workspace_root || !target.starts_with(workspace_root) {
        return false;
    }
    let mut rules = ignore::gitignore::GitignoreBuilder::new(workspace_root);
    for name in [".gitignore", ".ignore"] {
        let file = workspace_root.join(name);
        if file.is_file() {
            rules.add(file);
        }
    }
    rules.build().is_ok_and(|rules| rules.matched_path_or_any_parents(target, target.is_dir()).is_ignore())
}

fn scan_text_root(
    workspace_root: PathBuf,
    root: PathBuf,
    matcher: regex::Regex,
    scan: &ScanOptions,
) -> ScanFound {
    let mut found = ScanFound::default();
    let skipped_noise = Arc::new(AtomicBool::new(false));
    let skipped_noise_flag = Arc::clone(&skipped_noise);
    let allowed_noise = scan.allowed_noise.clone();
    let filter_globs = scan.globs.clone();

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .ignore(scan.respect_gitignore)
        .git_ignore(scan.respect_gitignore)
        .git_exclude(scan.respect_gitignore)
        .parents(scan.respect_gitignore)
        .filter_entry(move |entry| {
            if filter_globs.as_ref().is_some_and(|g| {
                g.is_excluded(
                    entry.path(),
                    entry.file_type().is_some_and(|ft| ft.is_dir()),
                )
            }) {
                return false;
            }
            if is_skipped_noise_dir(entry, &allowed_noise) {
                skipped_noise_flag.store(true, Ordering::Relaxed);
                return false;
            }
            true
        });

    'outer: for item in builder.build() {
        let entry = match item {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if let Some(globs) = &scan.globs
            && !globs.matches(path, false)
        {
            continue;
        }
        if crate::database::check_source_access(path).is_err() {
            continue;
        }
        let Ok(metadata) = fs::metadata(path) else {
            continue;
        };
        if metadata.len() > DEFAULT_SEARCH_FILE_LIMIT_BYTES {
            continue;
        }
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        // Paths use `/` on every platform, like list_dir and the
        // symbol index matches this list is merged with.
        let rel_path = path
            .strip_prefix(&workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        if scan.mode != SearchOutput::Content {
            let count = match scan.mode {
                SearchOutput::Files => usize::from(content.lines().any(|line| matcher.is_match(line))),
                _ => content.lines().filter(|line| matcher.is_match(line)).count(),
            };
            if count > 0 {
                if found.counts.len() >= scan.limit {
                    found.truncated = true;
                    break;
                }
                found.counts.push(FileCount { path: rel_path, count });
            }
            continue;
        }
        let lines = content.lines().collect::<Vec<_>>();
        let mut file_matches = 0usize;
        for (line_index, line) in lines.iter().enumerate() {
            if let Some(found_at) = matcher.find(line) {
                if scan.max_matches_per_file.is_some_and(|cap| file_matches >= cap) {
                    found.truncated = true;
                    break;
                }
                if found.matches.len() >= scan.limit {
                    found.truncated = true;
                    break 'outer;
                }
                file_matches += 1;
                let column = line[..found_at.start()].chars().count() + 1;
                let (text, line_chars) = match_excerpt(line, column - 1);
                let near = |range: std::ops::Range<usize>| {
                    lines[range].iter().map(|line| match_excerpt(line, 0).0).collect::<Vec<_>>()
                };
                found.matches.push(TextMatch {
                    path: rel_path.clone(),
                    line: line_index + 1,
                    column,
                    text,
                    line_chars,
                    before: near(line_index.saturating_sub(scan.context_lines)..line_index),
                    after: near(line_index + 1..(line_index + 1 + scan.context_lines).min(lines.len())),
                });
            }
        }
    }

    found.skipped_noise = skipped_noise.load(Ordering::Relaxed);
    found
}

/// A matched line as it is returned: whole when short, otherwise the part
/// around the match, so one long line (minified code, a JSON log record)
/// cannot fill a reader's context. `at` is the match's character offset.
fn match_excerpt(line: &str, at: usize) -> (String, Option<usize>) {
    let total = line.chars().count();
    if total <= MATCH_TEXT_CHARS {
        return (line.to_owned(), None);
    }
    let start = at
        .saturating_sub(MATCH_CONTEXT_CHARS)
        .min(total - MATCH_TEXT_CHARS);
    (
        line.chars().skip(start).take(MATCH_TEXT_CHARS).collect(),
        Some(total),
    )
}

fn query_indexed_symbol_table(
    conn: &rusqlite::Connection,
    workspace_root: &str,
    table: &str,
    lang: &str,
    query_name: &str,
    root_filters: &[String],
    max_matches: usize,
    matches: &mut Vec<TextMatch>,
    seen_symbols: &mut std::collections::HashSet<(String, usize)>,
) -> bool {
    let sql = format!(
        "SELECT kind, file_path, start_line, signature FROM {} WHERE name = ? AND workspace_root = ?",
        table
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return false;
    };
    let Ok(mut rows) = stmt.query(rusqlite::params![query_name, workspace_root]) else {
        return false;
    };

    while let Ok(Some(row)) = rows.next() {
        let kind: String = row.get(0).unwrap_or_default();
        let file_path: String = row.get(1).unwrap_or_default();
        let start_line: usize = row.get(2).unwrap_or(0);
        let signature: String = row.get(3).unwrap_or_default();

        if !path_allowed_by_filters(&file_path, root_filters) {
            continue;
        }
        if matches.len() >= max_matches {
            return true;
        }

        matches.push(TextMatch {
            path: file_path.clone(),
            line: start_line,
            column: 1,
            text: format!("[Symbol Definition ({} {})] {}", lang, kind, signature),
            line_chars: None,
            before: Vec::new(),
            after: Vec::new(),
        });
        seen_symbols.insert((file_path, start_line));
    }

    false
}

fn normalize_rel_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

fn path_allowed_by_filters(path: &str, filters: &[String]) -> bool {
    let path = normalize_rel_path(path);
    filters
        .iter()
        .any(|filter| filter == "." || path == *filter || path.starts_with(&format!("{}/", filter)))
}

fn map_ts_index_error(error: ts_index::TsIndexError) -> ToolError {
    ToolError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        error.to_string(),
    ))
}

fn map_rust_index_error(error: rust_index::RustIndexError) -> ToolError {
    ToolError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        error.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    include!("parts/tools_tests.inc.rs");
}
